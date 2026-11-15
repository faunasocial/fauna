using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Services;
using FaunaApp.Views;
// fauna-launch-machine FFI types, aliased one by one (the generated namespace is
// large). ServiceClients.SeedPendingInvite carries the launch machine's own
// PendingInviteRecord — the record the per-actor slot composes.
using LaunchMachine = uniffi.fauna_launch_machine.LaunchMachine;
using PendingInviteRecord = uniffi.fauna_launch_machine.PendingInviteRecord;
using LaunchPhase = uniffi.fauna_launch_machine.LaunchPhase;
using LaunchSnapshot = uniffi.fauna_launch_machine.LaunchSnapshot;
using LaunchWizardEntry = uniffi.fauna_launch_machine.LaunchWizardEntry;
using TokenStatus = uniffi.fauna_launch_machine.TokenStatus;
// The shared conversations session (libs/fauna-conversations), carried on
// ServiceClients so the conversations page renders off its wired manager.
using ConversationsSession = uniffi.fauna_conversations.ConversationsSession;

namespace FaunaApp;

/// <summary>
/// WinUI 3 application entry point. Connects to the Fauna nest server via HTTP.
/// All services (identity, messaging, contacts, backups, sync, bridges)
/// are accessed through a single DirectNestClient using Ed25519 auth.
/// </summary>
public partial class App : Application
{
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(System.IntPtr hwnd);

#if DEBUG || FAUNA_E2E_AGENT
    [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern System.IntPtr GetWindowLongPtr(System.IntPtr hwnd, int index);

    [System.Runtime.InteropServices.DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern System.IntPtr SetWindowLongPtr(System.IntPtr hwnd, int index, System.IntPtr value);

    private const int GWL_EXSTYLE = -20;
    private const long WS_EX_NOACTIVATE = 0x08000000L;
#endif

    /// <summary>Loopback port used to synthesize the <em>same-box</em> nest URL when
    /// neither the long-term store (a provisioned / paired <c>nest_url</c>) nor the
    /// <c>--nest-url</c> CLI override supplied one. This is the FIXED internal-loopback
    /// port (<c>fauna_protocol::node_policy::CANONICAL_INTERNAL_LOOPBACK_PORT</c> =
    /// 3000) the co-located nest binds for IPC alongside its external serving_port
    /// listener, so it never moves when the admin changes serving_port
    /// (nest/common.md § Same-box reach) — that is what keeps a port change from
    /// stranding this same-box app. The app trusts the nest's always-live self-signed
    /// floor served on this loopback via the shared pinned SPKI (DirectNestClient cert
    /// callback + the WS/FFI path). A <em>provisioned</em> remote <c>nest_url</c> is
    /// unaffected by this fallback.</summary>
    private const int DefaultNestPort = 3000;

    private Window? _window;
    public static Window? MainWindow { get; private set; }
    /// <summary>
    /// Factory-reset re-onboard hand-off, registered in <c>OnLaunched</c> and
    /// invoked by <see cref="Views.AdminSettingsPage"/> after
    /// <c>fauna.admin.factory_reset</c> returns the post-reset claim code.
    /// Args: <c>(nestUrl, handle, claimCode, secretHex)</c>. Tears down the
    /// authed session (keeping local creds — the box was wiped, not the client)
    /// and re-seeds onboarding at <c>claim_code</c> with the code pre-filled.
    /// Mirrors linux's <c>register_factory_reset_handler</c>. Per
    /// <c>docs/goal/behavior/mail-bridge-lifecycle.md</c> § Factory reset.
    /// </summary>
    internal static Action<string, string, string, string>? FactoryResetReonboardHandler;
    /// <summary>
    /// Sign-out hand-off, registered in <c>OnLaunched</c> and invoked by the
    /// Settings → Account <c>sign-out-confirm-button</c> (<see cref="Views.SettingsAccountPage"/>).
    /// Wipes the local credentials (the registry's <c>ClearAll</c>) and re-roots
    /// onboarding at <c>identity_choice</c> (the cold-start nav, no seed) — the
    /// user must re-create/import their secret key to sign back in. Unlike
    /// <see cref="FactoryResetReonboardHandler"/> (which keeps creds + pre-fills a
    /// claim code), sign-out is local-only and discards the stored identity.
    /// settings.md § Account (line 51). Mirrors linux <c>trigger_sign_out</c>.
    /// </summary>
    internal static Action? SignOutHandler;

    /// <summary>
    /// Switch the live session to another identity held by this install — the
    /// multi-account switcher's action (<c>long-term-store.md</c> § Multi-account
    /// evolution; switch-first design Decision 1: <c>set_active</c> → teardown →
    /// rebuild, LIVE IN-SESSION with no relaunch). Registered in
    /// <c>OnLaunched</c> beside <see cref="SignOutHandler"/> because the sequence
    /// needs the same captured scope (registry, rootFrame, account, baseUrl,
    /// the onboarding-completed closure).
    ///
    /// <para>Mirrors linux's <c>register_switch_account_handler</c>
    /// (<c>apps/fauna-linux/src/lib.rs</c>) in SEQUENCE, not in GTK specifics —
    /// crucially the mutate-BEFORE-teardown ordering, so a refused activation
    /// leaves the live session untouched.</para>
    /// </summary>
    internal static Func<string, bool, Task>? SwitchAccountHandler;

    /// <summary>
    /// Tear the live session down and re-enter launch over the SAME active account —
    /// the body every mid-session session-ending verdict performs
    /// (<see cref="Core.Services.SessionEndingRoute"/>; <c>security.md</c> § Post-auth
    /// surfacing). The fresh launch machine re-runs the challenge, earns the same
    /// refusal, and launch routing lands the right screen — the identity-import route
    /// for a supersession (<c>identity-succession.md</c> § Propagation → <i>Own device
    /// fleet</i>). Credentials are KEPT: the account moved, not the person. Registered
    /// in <c>OnLaunched</c> beside <see cref="SwitchAccountHandler"/>, whose teardown +
    /// rebuild it shares and whose re-entrancy guard it takes. The windows twin of
    /// FaunaKit's <c>escalateToLaunchSurface</c>.
    /// </summary>
    internal static Func<Task>? EscalateToLaunchSurfaceHandler;

    /// <summary>
    /// The native re-auth gate (Windows Hello) the account switcher consults before
    /// activating a <c>require_confirm_to_activate</c>-flagged account (Stage 2,
    /// <c>long-term-store.md</c> § Multi-account evolution → Per-account re-auth). Filled in
    /// <see cref="OnLaunched"/> from <see cref="Services.AccountReauth.ConfirmActivationAsync"/>
    /// — the app layer owns it because WinRT <c>UserConsentVerifier</c> is unreachable from
    /// <c>FaunaApp.Core</c>'s plain-<c>net10.0</c> TFM. Returns <c>true</c> on approval,
    /// <c>false</c> on decline / cancel / unavailable (fail-closed). Same role as apple's
    /// <c>AccountReauth.confirmActivation()</c>.
    /// </summary>
    internal static Func<System.Threading.Tasks.Task<bool>>? ConfirmReauthHandler;

    /// <summary>
    /// "Add account" — append-mode onboarding over the LIVE session
    /// (<c>long-term-store.md</c> § Multi-account evolution). Re-enters the existing
    /// onboarding wizard; on its <c>LoggedIn</c> outcome the completion closure
    /// appends the new identity to the registry and runs the SAME switch path, so
    /// there is no second single-identity boot.
    ///
    /// <para><b>Windows has no wizard close/cancel path to preserve.</b> Onboarding
    /// here is a frame-hosted <c>Page</c>, not a window: there is no close handler and
    /// no <c>Application.Exit</c> anywhere in the app, so — unlike linux/apple, whose
    /// append wizard is a separate toplevel that must be stopped from quitting the app
    /// — the risk is inverted. Backing out simply navigates back to the live session,
    /// which <see cref="AbandonAddAccountHandler"/> does.</para>
    /// </summary>
    internal static Action? AddAccountHandler;

    /// <summary>
    /// Back out of an in-progress append without disturbing the active account. An
    /// append-mode wizard writes nothing to the store before its own terminal, so
    /// there is nothing to heal; this just restores the running session's UI.
    /// </summary>
    internal static Action? AbandonAddAccountHandler;

    /// <summary>
    /// Re-entrancy guard for <see cref="SwitchAccountHandler"/> and
    /// <see cref="EscalateToLaunchSurfaceHandler"/>, the windows twin of
    /// linux's <c>switch_pending</c> <c>Cell&lt;bool&gt;</c>. A double-fire is a real
    /// hazard (a row click can arrive twice, and two rows can race two
    /// teardown/rebuilds), and the second one would tear down a session the first is
    /// still rebuilding. Reset on EVERY exit path, including the early refusals.
    /// </summary>
    private static int _switchPending;

    /// <summary>
    /// True while an append-mode ("Add account") wizard is running over a live
    /// session, which makes <c>onOnboardingCompleted</c> take the APPEND branch
    /// (<c>AddAccount</c> + switch) instead of the normal single-identity boot.
    /// </summary>
    private static bool _appendingAccount;

    /// <summary>
    /// Public read of <see cref="_appendingAccount"/> for <c>OnboardingPage</c>, which
    /// threads it into <c>OnboardingViewModel</c> at construction time so
    /// <c>identity_choice</c> can render <c>onboarding-cancel-button</c> only in append
    /// mode. FaunaApp.Core (where the VM lives) has no reference to this WinUI project,
    /// so the VM can't read <c>App</c> directly — the value must be injected, same
    /// pattern as <see cref="ConfirmReauthHandler"/>.
    /// </summary>
    internal static bool IsAppendingAccount => _appendingAccount;

    /// <summary>
    /// Unconditionally clears <see cref="_appendingAccount"/> — called from
    /// <c>OnboardingPage.OnNavigatedFrom</c>, which fires on EVERY exit from the
    /// wizard page (login-done, abandon-cancel, and any anomaly early-return
    /// alike), so the latch can never survive a page-away navigation. Without
    /// this, an exit path that forgets its own explicit reset (the missing
    /// secret/nest_url anomaly branch below did) leaves a STALE true behind: the
    /// next time the user opens a genuinely FRESH single-identity wizard, its
    /// moment-1 commit would wrongly take the append arm and silently lose the
    /// generated secret instead of registering it. A no-op when already false.
    /// </summary>
    internal static void ClearAppendingAccount() => _appendingAccount = false;

    /// <summary>
    /// The signed-in account's actor-id hex, or null when no session is up. Set at
    /// the universal post-auth hook (<see cref="StartMainAppAsync"/>, which every
    /// login AND account switch funnels through) and cleared at sign-out. It is
    /// the SAME source the per-session MLS store derives from
    /// (<c>NestRpcClient</c> uses <c>ICryptoService.ActorIdHex</c>), so drafts and
    /// MLS always scope on one identity. account-scoping.md § Serialized switching.
    /// </summary>
    internal static string? ActiveActorHex { get; private set; }

    /// <summary>
    /// Draft-persistence v2 for the conversations rail (docs/goal/behavior/file-sync.md
    /// § Drafts Sync): built at login over the shared <c>fauna.drafts.{get,put}</c> seam
    /// + the session's <c>ConversationsManager</c>. Nest-backed + cross-device. Null
    /// before login / under the E2E bridge; the conversations page schedules a
    /// debounced save on it after a compose change.
    /// </summary>
    internal static ConversationDraftsService? ConvDrafts { get; private set; }

    /// <summary>
    /// Draft-persistence v2 for the feed (<c>"posts"</c>) rail
    /// (docs/goal/behavior/reserved-folders.md § Drafts Sync; docs/goal/ui/feed.md §
    /// Persistence): the sync handle built once at login (<see cref="FeedDraftsSync"/>)
    /// stays fixed for the session, and <see cref="FfiFeedManager"/> is the session's
    /// one (<c>FeedManagerHost</c>) — so <see cref="FeedDrafts"/> is built and
    /// restored once per (manager, sync handle) pair by
    /// <c>FeedDraftsService.EnsureAsync</c> from <c>FeedPage.Page_Loaded</c>, never on
    /// a repeat load, and kept reachable here for the teardown flush
    /// (<c>FlushDraftRailsAsync</c>). Null before login, before the feed page
    /// has ever loaded, or under the E2E bridge.
    /// </summary>
    internal static uniffi.fauna_ffi.IFfiDraftsSync? FeedDraftsSync { get; private set; }

    /// <inheritdoc cref="FeedDraftsSync"/>
    internal static FeedDraftsService? FeedDrafts { get; set; }

    /// <summary>
    /// Draft-persistence v2 for the events (<c>"events"</c>) rail
    /// (docs/goal/behavior/reserved-folders.md § Drafts Sync; docs/goal/ui/events.md §
    /// Persistence — the windows arm): the sync handle built once at login (<see
    /// cref="EventDraftsSync"/>) stays fixed for the session, but the full service
    /// pairs with <c>EventsPage</c>'s compose surface — which does not exist yet at
    /// login (built lazily on first Events-page visit, like <see cref="FeedDrafts"/>'s
    /// <c>FfiFeedManager</c>) — so <c>EventsPage</c> itself builds this the first time
    /// it loads. Like <see cref="FeedDrafts"/> it is built only ONCE per session,
    /// never rebuilt on re-entry: <c>EventsPage</c> is <c>NavigationCacheMode.Required</c>
    /// and its <c>TextBox</c>es are never torn down, so one instance covers the page's
    /// whole cached lifetime. Null before login, before Events has ever loaded, or
    /// under the E2E bridge.
    /// </summary>
    internal static EventDraftsService? EventDrafts { get; set; }

    /// <inheritdoc cref="EventDrafts"/>
    internal static uniffi.fauna_ffi.IFfiEventDraftsSync? EventDraftsSync { get; private set; }

    /// <summary>
    /// The e2e login's fire-and-forget <see cref="WireEventDraftsAsync(NestRpcClient)"/>
    /// kickoff (set_state never reaches <see cref="StartMainAppAsync"/>, where production
    /// awaits it synchronously before login completes — same reasoning as
    /// <c>_e2eFeedDraftsSyncTask</c>, but Events is not the default post-login view, so
    /// there is no single pre-navigation choke point to bound-await it from; instead
    /// <c>EventsPage</c>'s own first load bound-awaits THIS field directly, whenever it
    /// happens to run. Always null under production (where the field is never set,
    /// because the await there already completed before login finished) and under
    /// Release e2e-agent builds this stays reachable — unlike the conversations/feed
    /// e2e task fields, this one is not <c>#if DEBUG</c>-gated, since the page that reads
    /// it is unconditional.
    /// </summary>
    internal static Task? EventDraftsSyncTask { get; private set; }

    private DirectNestClient? _nestClient;

    /// <summary>
    /// The nest HTTP client the app is using <em>right now</em>.
    /// <para>Load-bearing for anything that outlives a single
    /// <see cref="ServiceClients"/> hand-off: <see cref="DisposeNestClients"/> disposes
    /// and replaces <see cref="_nestClient"/> whenever the session or nest URL changes
    /// (re-login, onboarding hand-off, factory-reset re-onboard), so a component that
    /// <em>captured</em> the instance it was handed keeps calling a DISPOSED
    /// <c>HttpClient</c> — which throws <c>ObjectDisposedException</c> and, in a
    /// swallow-and-skip consumer, fails silently. Resolve through here per call instead
    /// of holding the reference, and staleness becomes unrepresentable.</para>
    /// </summary>
    internal static INestHttpClient? CurrentNest => (Current as App)?._nestClient;

    /// <summary>
    /// The identity the launch machine serving this window last published — its latest
    /// verify reply's handle and domain — or <c>null</c> before one resolved (or with no
    /// machine, as an e2e state-protocol login has none). Read per call through the
    /// observer the app wired onto the machine it built last, so an account switch's new
    /// machine is followed without re-wiring. The profile SELF header reads it over the
    /// registry cache (<see cref="Core.Services.SelfDisplayHandle"/>): the reply is
    /// published, not persisted, so after a mid-session primary rename only it carries
    /// the new primary.
    /// </summary>
    internal static uniffi.fauna_launch_machine.LaunchIdentity? CurrentLaunchIdentity =>
        (Current as App)?._selfAddressHealObserver?.Machine?.Snapshot().@identity;

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// This session's live WS-RPC client, or <c>null</c> before login / after
    /// teardown. Resolved per call for the same staleness reason as <see
    /// cref="CurrentNest"/> — a login rebuilds <c>_rpcClient</c>. Used by the
    /// <c>family_notify_check_now</c> e2e command (testing.md convention 14),
    /// which has no page-scoped ViewModel to reach through — its only caller, so
    /// it compiles out of Release with the agent (convention 15).
    /// </summary>
    internal static Core.Services.INestRpcClient? CurrentRpc => (Current as App)?._rpcClient;
#endif

    /// <summary>
    /// This session's live sync-agent control channel, or <c>null</c> when none
    /// is provisioned (no agent, e2e without <c>FAUNA_E2E_REAL_SYNC_AGENT</c>, or
    /// signed out). Resolved per call for the same staleness reason as
    /// <see cref="CurrentNest"/> — the session is rebuilt on every login and
    /// dropped on every teardown.
    /// </summary>
    internal static FaunaApp.Core.Services.SyncAgentSession? CurrentSyncAgent =>
        (Current as App)?._syncAgent.Current;

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// Whether a sync-agent session install is still in flight — the causal barrier the
    /// unprovision e2e anchors its negative assert to, in place of a settle window (see
    /// <see cref="FaunaApp.Core.Services.SyncAgentSessionHost{T}.InstallInFlight"/>).
    /// The e2e state serializer is its only reader (convention 15).
    /// </summary>
    internal static bool SyncAgentInstallInFlight =>
        (Current as App)?._syncAgent.InstallInFlight ?? false;
#endif

    /// <summary>
    /// This login's folder-binding control plane — the optimistic model every binding
    /// surface reads and writes (<c>data.sync.locations</c>, the Folders page's nested rows,
    /// the e2e <c>sync_add_location</c>/<c>sync_remove_location</c> commands). Non-null from the
    /// moment login starts the hydration session, and <b>deliberately not tied to
    /// <see cref="CurrentSyncAgent"/></b>: the agent session takes as long as its nest
    /// round-trips take to install, and a binding made in that window must be recorded and
    /// rendered rather than dropped. The controller pushes it the instant the session
    /// attaches. Null only before login and after teardown.
    /// </summary>
    internal static FaunaApp.Core.Services.LocationBindingsController? CurrentLocationBindings =>
        (Current as App)?._locationBindings;

#if P2P_SHARE
    /// <summary>
    /// This login's offline co-present share ceremony seat (p2p.md § Offline
    /// share initiation) — bound lazily by the Folders page the first time
    /// either offline-share panel opens, and held HERE rather than on the page
    /// instance because the page has no <c>NavigationCacheMode</c> (a fresh
    /// instance per navigation): the recipient's "did the invitation land"
    /// polling re-navigates to the Folders page repeatedly
    /// (<c>test_offline_share_two_seat.py::_consent_card_count</c>), which would
    /// destroy an armed <c>ExpectFrom</c> listener the instant the old page
    /// instance were collected. Torn down (disposed + reset to Closed/Idle)
    /// alongside <see cref="_locationBindings"/> on sign-out / reset / switch —
    /// a stale seat bound under a previous account's secret must not survive
    /// into the next login.
    /// </summary>
    internal static uniffi.fauna_ffi.FfiCeremonySeat? CurrentOfflineShareSeat
    {
        get => (Current as App)?._offlineShareSeat;
        set { if (Current is App app) app._offlineShareSeat = value; }
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>The signed-in identity secret, for the e2e agent's
    /// <c>offline_share_probe_set</c> arm only (the FFI door is keyed by the owner
    /// secret like the ceremony's own doors). Compiled out of release artifacts —
    /// convention 15: a secret accessor is never a runtime-gated production face.</summary>
    internal static byte[]? CurrentCryptoSecretForTest =>
        (Current as App)?._cryptoService is { HasKey: true } c ? c.SecretBytes : null;
#endif

    /// <summary>Which offline-share panel is open, alongside <see cref="CurrentOfflineShareSeat"/>
    /// for the same navigation-survival reason.</summary>
    internal static uniffi.fauna_client_capabilities.OfflineSharePanel CurrentOfflineSharePanel
    {
        get => (Current as App)?._offlineSharePanel ?? uniffi.fauna_client_capabilities.OfflineSharePanel.Closed;
        set { if (Current is App app) app._offlineSharePanel = value; }
    }

    /// <summary>The ceremony's last-known status, alongside <see cref="CurrentOfflineShareSeat"/>
    /// for the same navigation-survival reason.</summary>
    internal static uniffi.fauna_client_capabilities.CeremonyStatus CurrentOfflineShareStatus
    {
        get => (Current as App)?._offlineShareStatus ?? uniffi.fauna_client_capabilities.CeremonyStatus.Idle;
        set { if (Current is App app) app._offlineShareStatus = value; }
    }

    /// <summary>The actor id last armed via <c>FfiCeremonySeat.ExpectFrom</c> on
    /// the RECEIVE panel — held so Cancel can withdraw the same expectation
    /// (<c>CancelExpectation</c>) even after a re-navigation recreated the page
    /// that originally read the peer-code box.</summary>
    internal static byte[]? CurrentOfflineShareExpectingFrom
    {
        get => (Current as App)?._offlineShareExpectingFrom;
        set { if (Current is App app) app._offlineShareExpectingFrom = value; }
    }
#endif

    /// <summary>
    /// The remote-change nudge relay for a new <see cref="NestRpcClient"/>
    /// (file-sync.md § Remote-change nudge). Resolves the session per call, so an RPC
    /// client built before the agent session exists — or surviving across a re-login —
    /// still nudges the CURRENT agent rather than a captured dead one.
    ///
    /// <para>Every <c>NestRpcClient</c> construction site must pass this: the push pump
    /// is started from more than one place, and a relay wired at only some of them
    /// reproduces silently as "sync is slow" (the  regression).</para>
    /// </summary>
    private static FaunaApp.Core.Services.IAgentSyncNudge SyncNudge() =>
        new FaunaApp.Core.Services.AgentSyncNudge(() => CurrentSyncAgent?.Channel);

    // WS-RPC request/reply plane (events today; more clusters as they migrate),
    // built alongside _nestClient and threaded into MainPage-bound ServiceClients.
    private NestRpcClient? _rpcClient;

    /// <summary>
    /// How long a session teardown waits for <see cref="FlushDraftRailsAsync"/> before
    /// it disposes the transport anyway. Same reasoning as
    /// <see cref="Core.Services.AccountRuntimeStops.StopBudget"/>: a healthy flush is one sealed PUT per
    /// changed rail and lands in well under a second, so only a dead connection
    /// spends this — and a sign-out that hung on one would be worse than one that
    /// loses the last edit.
    /// </summary>
    private static readonly TimeSpan TeardownDraftFlushBudget = TimeSpan.FromSeconds(5);

    /// <summary>
    /// The leave-flush (<c>reserved-folders.md</c> § The leave-flush promise): hand every
    /// draft rail's not-yet-saved edit to the nest NOW instead of at the end of its
    /// debounce. The one body behind every leave door — the tray quit and the
    /// session-end (<see cref="Services.TrayIconService.QuitApplication"/>,
    /// <see cref="Services.RestartManagerService"/>), sign-out, the account switch, and
    /// the e2e <c>reset</c>/<c>logout</c> arms (the last three through
    /// <see cref="SignOutHandler"/> and <see cref="DisposeNestClients"/>) — so no door can
    /// flush two rails and forget the third.
    ///
    /// <para>Each rail reads its snapshot synchronously, inside this call, before its
    /// first await: a caller that drops the managers or clears a composer right after
    /// it still flushes what the user left. What the caller owes in return is the
    /// transport — the PUTs ride the session's live client, so the client must outlive
    /// the returned task. A teardown that disposed it first is what lost the last edit:
    /// the debounce fired on a disposed client ("The connection to the nest was lost"),
    /// and the next sign-in restored the draft from before it.</para>
    ///
    /// <para>Never faults: each rail swallows its own failure (best-effort, like the
    /// debounced save it stands in for).</para>
    /// </summary>
    internal static Task FlushDraftRailsAsync()
    {
        var flushes = new List<Task>(3);
        void Start(Func<Task?> flush)
        {
            try
            {
                if (flush() is { } started) flushes.Add(started);
            }
            catch (Exception ex)
            {
                ShellLog.Warn("App", $"[drafts] leave-flush could not start a rail: {ex.Message}");
            }
        }
        Start(() => ConvDrafts?.SaveNowAsync());
        Start(() => FeedDrafts?.FlushIfPendingAsync());
        Start(() => EventDrafts?.FlushIfPendingAsync());
        return Task.WhenAll(flushes);
    }

    /// <summary>Wait for a started <see cref="FlushDraftRailsAsync"/>, bounded by
    /// <see cref="TeardownDraftFlushBudget"/> — the teardown's half of the contract
    /// above: the transport goes only after this returns.</summary>
    private static async Task AwaitDraftFlushAsync(Task flush)
    {
        try
        {
            await flush.WaitAsync(TeardownDraftFlushBudget).ConfigureAwait(false);
        }
        catch (TimeoutException)
        {
            ShellLog.Warn("App", "[drafts] leave-flush did not finish within "
                + $"{TeardownDraftFlushBudget.TotalSeconds:F0}s — tearing the session down anyway");
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[drafts] leave-flush failed: {ex.Message}");
        }
    }
    private CryptoService? _cryptoService;
    // The post-onboarding hand-off (assigned in OnLaunched once the closure over
    // account / rootFrame / window exists). Held as a field so the e2e
    // `reset`/`logout` test commands — which re-navigate to OnboardingPage to
    // drive a fresh wizard — can wire it onto the VM exactly as a real launch
    // does (DispatchLaunchSnapshotAsync), so a post-reset onboarding run still
    // re-points the main nest client to the onboarded URL and lands on the main
    // app. Without it, OnboardingPage sets ViewModel.OnLoggedIn = null and the
    // wizard's LoggedIn never re-points the client (stays on the launch-default
    // `DefaultNestPort` fallback) — the cause of the test_onboarding_dns_glue_windows
    // seal-readback failure (the seal itself succeeds; the admin-dns read can't
    // reach the nest).
    private System.Action? _onOnboardingCompleted;
    // The live post-auth refresh context RunTtlRefreshLoopAsync runs over — held so
    // TriggerSilentSignInForTestAsync (e2e bridge only) can force the SAME on-demand
    // refresh the loop's own schedule performs, instead of waiting for its near-expiry
    // wakeup (security.md § Post-auth surfacing).
    // Set at the loop's one spawn site (the "transition into Online" call), cleared in
    // DisposeNestClients alongside every other actor-scoped session field.
    private (LaunchMachine Machine, Frame RootFrame, ISessionAccount Account, string SecretHex, string BaseUrl)?
        _liveTtlRefreshContext;
    // App-lifetime DM OS-toast observer over the login-built ConversationsManager;
    // held so the UniFFI SnapshotObserver callback survives for the app's lifetime.
    private FaunaApp.Conversations.MessageToastObserver? _messageToastObserver;
    // conversations.md § State & data shape → Self-address: live, never baked — the real
    // LaunchObserver wired onto the ACTIVE LaunchMachine (replacing NullLaunchObserver at
    // all four construction sites), pushing SetSelfAddress on every resolved/changed
    // identity. Created alongside each new LaunchMachine (before Start()); StartMainAppAsync
    // attaches the freshly-built ConversationsSession once it exists.
    private Core.Services.SelfAddressHealObserver? _selfAddressHealObserver;
    // The PRODUCTION ConversationsSession built by StartMainAppAsync, held here so
    // the actor-change teardown has something to CLOSE. Until this field existed the
    // production session lived only inside the ServiceClients record handed to
    // MainPage, so a sign-out dropped the last C# reference without ever disposing
    // the UniFFI object — see DisposeNestClients for what that costs.
    private ConversationsSession? _liveConvSession;
    // E2E-only: the real shared-Rust ConversationsSession built in the set_state
    // login path. Held so the actor-change teardown has something to close — the C#
    // twin of linux's conv_backend ACTIVE_SESSION holder. Null in production (that
    // path holds the session via ServiceClients.ConvSession).
    //
    // ⚠ This comment used to say the field kept "its detached receive loop's
    // liveness `Weak` upgradeable (dropping the Arc stops the loop)". Both halves
    // are stale: the polled `Weak<()>` was replaced 2026-08-27 by a session-closed
    // `watch` the loop `select!`s on (fauna-conversations/src/session.rs), so the
    // loop ends on the EVENT, not the next tick — and dropping this Arc does not
    // stop anything by itself, because the Rust client holds its own stashes of the
    // same session. Releasing the store is an explicit, refcount-independent
    // hand-over (NestRpcClient.ReleaseAccountScopedStoresAsync), never a matter of
    // who dropped what.
#if DEBUG || FAUNA_E2E_AGENT
    private uniffi.fauna_conversations.ConversationsSession? _e2eRealConvSession;
    // The recording device that session was built under — the one the e2e
    // serve action hands `FoldersServeSetAsync`, as the production page does.
    private string? _e2eRealConvDeviceIdHex;
#endif
    // E2E-only: the in-flight build of the above, kicked off on authentication and
    // awaited by the nav actions so ServiceClients.ConvSession carries a REAL session
    // — exactly like production's StartMainAppAsync. Without it every _convSession-
    // gated gesture (folder share/leave/serve/paywall) silently no-ops under e2e,
    // because the set_state login never reaches StartMainAppAsync.
#if DEBUG || FAUNA_E2E_AGENT
    private Task<uniffi.fauna_conversations.ConversationsSession?>? _e2eConvSessionTask;
    // Same "kick off early, bounded-await inside navAction" shape as
    // _e2eConvSessionTask above, for the feed-drafts sync handle
    // (WireFeedDraftsAsync) — without the bounded await, FeedPage.Page_Loaded
    // (the default post-login view) can race ahead of App.FeedDraftsSync still
    // being null, silently skipping restore for that navigation.
    private Task? _e2eFeedDraftsSyncTask;
#endif
    // Session-scoped sync-agent provisioning convergence loop (agent running +
    // provisioned + fresh bearer), started from BOTH login seams and stopped in
    // DisposeNestClients. Replaces the once-at-login ProvisionCapability push — a
    // restarted agent re-provisions within one tick, and the e2e set_state seam can
    // reach the spawn→provision→hydration chain (apps/windows.md § On-demand
    // hydration host; on-demand-files.md § On-Demand Files).
    // The shared-Rust provisioner (sync-agent.md § Consumers) plus its pushed-event
    // listener, which the C# convergence twin retired onto. Its sync-complete toast
    // consumer is started/stopped with the session itself.
    // The session LIFECYCLE (install / supersede / teardown) lives in the host, not
    // inline here: it owns the generation, the publish-before-start ordering and the two
    // teardown causes, and it is unit-tested (SyncAgentSessionHostTests) precisely because
    // the race it arbitrates is invisible from this class. See its own docs for the
    // invariant — a session that has PROVISIONED is always reachable by teardown.
    private readonly FaunaApp.Core.Services.SyncAgentSessionHost<FaunaApp.Core.Services.SyncAgentSession> _syncAgent = new();
    // The folder-binding control plane for this login. Built SYNCHRONOUSLY by
    // StartHydrationSession, i.e. ~10 s before the session exists, because a
    // binding made in that window must survive (see CurrentLocationBindings).
    private FaunaApp.Core.Services.LocationBindingsController? _locationBindings;
#if P2P_SHARE
    // Offline co-present share ceremony session state — see CurrentOfflineShareSeat.
    private uniffi.fauna_ffi.FfiCeremonySeat? _offlineShareSeat;
    private uniffi.fauna_client_capabilities.OfflineSharePanel _offlineSharePanel =
        uniffi.fauna_client_capabilities.OfflineSharePanel.Closed;
    private uniffi.fauna_client_capabilities.CeremonyStatus _offlineShareStatus =
        uniffi.fauna_client_capabilities.CeremonyStatus.Idle;
    private byte[]? _offlineShareExpectingFrom;
#endif
    // "The authenticated main app is mounted RIGHT NOW" — derived from the one
    // fact that can never go stale, which page the root frame is actually showing
    // (the `rootFrame.Navigated` hook in OnLaunched is its only writer). Two
    // readers, both of which need exactly this and not "an identity key exists":
    //
    //   * the --autostart hidden-launch gate (apps/windows.md § App Lifecycle →
    //     Auto-start at sign-in): only a launch that actually landed Online may
    //     stay tray-resident; an onboarding route always shows the window.
    //   * the e2e state protocol's `session.authenticated`
    //     (e2e-conventions.md § convention 11 → *what `session.authenticated`
    //     means*), where stored credentials are emphatically NOT a session.
    //
    // Derived rather than assigned at the login seam on purpose: a flag set by
    // whoever remembers to set it has to be cleared by everyone who remembers to
    // clear it — sign-out, factory reset, account-abandon, a failed switch, a
    // launch that throws before it mounts — and the one that forgets publishes a
    // lie forever after. Navigation is the fact all of those already go through.
    // volatile: written on the UI thread, read by SerializeState on the agent's
    // poll thread.
    private volatile bool _mainAppMounted;
    private string? _handle;
    private string? _deviceId;
    // Test agent tracks the current view name independently of the UI thread,
    // so SerializeState can run on any thread without blocking on XAML.
#if DEBUG || FAUNA_E2E_AGENT
    private volatile string _testCurrentView = "welcome";

    /// <summary>
    /// The shell's top-level page changed — by a user's tab press, a keyboard
    /// activation, a shell's back button, or a <c>nav</c> patch alike. Keeps
    /// <c>nav.stack[0].view</c> the page ON SCREEN: set only from <c>nav</c>
    /// patches, it read "feed" for a whole walk the user took through the tabs
    /// (convention 11 — a stale answer reads as a product bug). Called from
    /// <c>MainPage.NoteViewWhenLoaded</c> once the selected page has loaded, on the
    /// UI thread.
    /// </summary>
    internal static void NoteShellView(string view)
    {
        if (Current is App app) app._testCurrentView = view;
    }
    // An explicit `set_state({"session": {"authenticated": …}})` override, which
    // WINS over _mainAppMounted while it is set — the exact shape of linux's
    // `SessionOverride::authenticated` (`apps/fauna-linux/src/lib.rs`, read at
    // its `authenticated_override.unwrap_or(app_state.is_some())`). It is what
    // keeps the ordinary e2e login honest: that path (the `session` block in
    // HandleTestCommand) navigates to MainPage from a DEFERRED post-action and
    // never reaches StartMainAppAsync, so a test that logs in and reads state in
    // the same breath must not see a transient `false`. Cleared by reset/logout,
    // like linux's. Boxed bool so "no override" is distinguishable from `false`.
    private volatile object? _testAuthenticatedOverride;
    // This launch's window-activation DECISION, published once the --autostart
    // gate at the foot of OnLaunched has run. Boxed bool, null until then, and
    // the null is the point: the e2e case asserts a NEGATIVE ("an --autostart
    // launch that lands Online opens no window"), which convention 14 forbids
    // anchoring to a settle-sleep — so the state key's PRESENCE is the causal
    // barrier (published strictly after Activate() was called or skipped) and
    // its VALUE is the assertion. Same boxed-null idiom as
    // _testAuthenticatedOverride above, for the same "absent ≠ false" reason.
    // volatile: written on the UI thread, read by SerializeState on the agent's
    // poll thread.
    private volatile object? _launchWindowShown;
    // Whether this process saw `--autostart` at all, published alongside the
    // decision so the test can prove it drove the arm it meant to: a flag lost
    // between the harness and Environment.GetCommandLineArgs() would otherwise
    // present as a perfectly green "the window was shown" arm.
    private volatile object? _launchAutostart;
    // Whether ShowLaunchWindow ACTIVATED the window (true) or only showed it
    // (false, the harness's --e2e-no-activate launch). Null until a window was
    // shown at all. Published beside _launchWindowShown so the no-focus-steal
    // case (tests/e2e-unified/tests/test_windows_no_focus_steal.py) can tell "the
    // app took the no-activate branch" from "the OS happened not to grant it".
    private volatile object? _launchWindowActivated;
    // The harness's launch argument for e2e convention 10's windows focus axis:
    // show the window, never activate it, so a run never takes the keyboard focus
    // from the person working in this Windows session. Test-agent builds only —
    // a shipping build has no such argument (convention 15).
    internal const string E2eNoActivateArg = "--e2e-no-activate";
#endif

    /// <summary>
    /// Put the launch window on screen. A shipping build ACTIVATES it — a person
    /// who launched the app wants it in front. A test-agent build launched with
    /// <see cref="E2eNoActivateArg"/> SHOWS it without activation
    /// (<c>AppWindow.Show(activateWindow: false)</c>): every UIA gesture the e2e
    /// bridge drives works on an inactive window, and activation is what took the
    /// keyboard focus from an RDP user sharing this session on every one of a
    /// run's dozens of launches (e2e convention 10's windows focus axis,
    /// axis (d), <c>docs/goal/architecture/e2e-launch-isolation.md</c>). The two activation sites
    /// — the launch-instance chooser and the foot of <c>OnLaunched</c> — both come
    /// through here, so neither can drift back to an unconditional Activate().
    /// </summary>
    private void ShowLaunchWindow()
    {
#if DEBUG || FAUNA_E2E_AGENT
        if (Array.IndexOf(Environment.GetCommandLineArgs(), E2eNoActivateArg) >= 0)
        {
            // Not activating at launch is not enough on its own: WinUI's text
            // automation peer focuses its control before a ValuePattern SetValue,
            // and focusing a control in an inactive window activates it — measured
            // by the no-focus-steal case, whose bridge record named
            // `ValuePattern.SetValue on compose-text-field`. WS_EX_NOACTIVATE makes
            // the window refuse activation as a SIDE EFFECT, while an explicit
            // SetForegroundWindow (the bridge's physical-input fallbacks) still wins.
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_window);
            var ex = GetWindowLongPtr(hwnd, GWL_EXSTYLE).ToInt64();
            SetWindowLongPtr(hwnd, GWL_EXSTYLE, new System.IntPtr(ex | WS_EX_NOACTIVATE));
            _window!.AppWindow.Show(activateWindow: false);
            _launchWindowActivated = false;
            return;
        }
        _launchWindowActivated = true;
#endif
        _window!.Activate();
    }
    private static volatile string _testInboxMode = "";
    // Current error message from whatever page is active. Written by pages on UI thread,
    // read by SerializeState on thread-pool thread — volatile for safe cross-thread reads.
    // Static because pages reference it as App.CurrentErrorMessage (WinUI 3 has one App instance).
    private static volatile string? _currentErrorMessage;
    private static volatile string? _currentWarningMessage;
    // Deliberately NOT `#if DEBUG || FAUNA_E2E_AGENT`-gated, unlike the other test-only
    // fields around here: production READS it (the custody-warning hand-off passes it to
    // MainPage.UpdateTestMessages), so gating the field would break a production call
    // site. Only the e2e command table ever WRITES it, so a Release build warns CS0649
    // "never assigned" — expected and correct, not drift: it is the compiler stating the
    // true fact that nothing outside the e2e path sets an info message today.
    private static volatile string? _currentInfoMessage;
#if DEBUG || FAUNA_E2E_AGENT
    // The test agent's OWN failure slot — `e2e-conventions.md` § convention 11's
    // build-out record, whose one hard requirement is the slot's SHAPE: "a
    // **dedicated, page-independent, nav-independent** slot cleared at `reset()`".
    // `_currentErrorMessage` above is none of those three: every page writes it as
    // it opens/closes its own error bar, and several clear it to null in their load
    // path (AdminDashboardPage, AdminBridgesPendingPage, AdminDnsPage, …), so a
    // refusal stamped there is wiped by the very next navigation — and `navigate_to`
    // IS a nav, which every action-layer helper issues. Writing the refusal into the
    // page mirror is the FIRST WRONG SLOT tui, android and apple each shipped before
    // getting here, and windows shipped it too: every `App.CurrentErrorMessage = …`
    // in `Testing/TestAgent.cs` wrote the page mirror until 2026-08-29.
    //
    // Written only by TestAgent's funnel (itself wholly inside this same gate), read
    // only by SerializeState below, which publishes it into `messages.error` AHEAD of
    // the page mirror: a refused or failed agent command means the app never did what
    // the driver asked, so any later product assertion is reading a state the test did
    // not set up. Cleared in exactly one place — HandleTestCommand's `reset`/`logout`
    // arms — which is the per-test boundary every `app` fixture drives, so a refusal
    // can never leak forward into a test that did not cause it. Twins: tui
    // `App::refused_agent_command`, linux `SharedState::agent_command_failure`,
    // android + apple `AppMessages.refusedAgentCommand`.
    //
    // volatile for the same reason as the fields above: written on the agent poll
    // thread (and on the UI thread, for post-action failures), read by SerializeState
    // from the thread pool.
    private static volatile string? _agentCommandFailure;

    /// <summary>
    /// The nav-independent agent-failure slot (convention 11). Null when the agent has
    /// refused nothing since the last <c>reset</c>.
    /// </summary>
    internal static string? AgentCommandFailure
    {
        get => _agentCommandFailure;
        set => _agentCommandFailure = value;
    }

    // The e2e `messages.error` injection's OWN slot — tui's `App::injected_error`.
    // `set_state({"messages": {"error": …}})` wrote the page mirror
    // `_currentErrorMessage` until 2026-09-13: the wrong slot again, for the same
    // reason the refusal above could not live there. Pages rewrite the mirror as
    // they re-render, and FeedPage.Refresh() — run on EVERY feed observer tick —
    // nulls it whenever the snapshot carries no error, so an injection read back as
    // null whenever a tick landed between the patch and the read: a half-applied
    // command (convention 11's corollary), which a batch run hit 3/3 and a solo run
    // missed. Its lifetime is the injected message's, never a page's: written by the
    // `messages` patch (a null clears it), and cleared by the `nav` and `session`
    // patches — a nav dismisses transient messages, as on tui and linux — and by
    // `reset`/`logout`. SerializeState reads it after the agent-failure slot and
    // ahead of the page mirror. Pinned by
    // `test_messages.py::test_injected_error_survives_a_page_refresh`.
    //
    // volatile: written on the agent poll thread, read by SerializeState from the
    // thread pool.
    private static volatile string? _injectedErrorMessage;

    // The most recent `serve_enable_folder` test-agent command's outcome —
    // `{"ok": true, "served_sets": N}` or `{"ok": false, "error": "…"}` — the
    // windows twin of linux's `SharedState::webdav_serve_reply`
    // (`apps/fauna-linux/src/test_agent.rs`). Cleared to null at the START of
    // each `serve_enable_folder` invocation (TestAgent's case), so the Python
    // driver's poll loop (`helpers/webdav_roundtrip.py::serve_enable_folder`)
    // can tell "this run's reply" from a stale one left by an earlier call.
    // volatile for the same cross-thread reason as `_agentCommandFailure`.
    private static volatile object? _webdavServeReply;
    internal static object? WebdavServeReply
    {
        get => _webdavServeReply;
        set => _webdavServeReply = value;
    }

    // The most recent `enable_caldav_mailbox` test-agent command's outcome —
    // `{"ok": true}` or `{"ok": false, "error": "…"}` — the windows twin of
    // linux's `SharedState::caldav_mailbox_reply` (`apps/fauna-linux/src/
    // test_agent.rs`), same shape as `WebdavServeReply` above. Cleared to null
    // at the START of each `enable_caldav_mailbox` invocation (TestAgent's
    // case), so the Python driver's poll loop
    // (`helpers/mail_dedicated_nest.py::mint_caldav_mailbox`) can tell "this
    // run's reply" from a stale one left by an earlier call. volatile for the
    // same cross-thread reason as `_agentCommandFailure`.
    private static volatile object? _caldavMailboxReply;
    internal static object? CaldavMailboxReply
    {
        get => _caldavMailboxReply;
        set => _caldavMailboxReply = value;
    }
#endif
    /// <summary>
    /// The currently displayed error message (from the active page's InfoBar).
    /// Set by page code-behind whenever ErrorBar.Message changes.
    /// Null when no error is shown.
    /// </summary>
    internal static string? CurrentErrorMessage
    {
        get => _currentErrorMessage;
        set
        {
            // Pages rewrite the mirror on every tick of a machine they observe, mostly
            // with the value it already holds — only a CHANGE is news.
            if (string.Equals(_currentErrorMessage, value, System.StringComparison.Ordinal)) return;
            // "Which page erased the error the page on screen just set" is a recurring
            // question with one mirror and every page writing it: under the e2e agent
            // log each change names its writer (the first frames past this setter).
            if (Core.Logs.E2eTrace.Enabled)
            {
                var frames = new System.Diagnostics.StackTrace(1, false).GetFrames()
                    .Take(3)
                    .Select(f => f.GetMethod() is { } m ? $"{m.DeclaringType?.Name}.{m.Name}" : "?");
                Core.Logs.E2eTrace.Write(
                    $"[error-mirror] {(_currentErrorMessage is null ? "null" : "set")} -> {(value is null ? "null" : $"\"{value}\"")} by {string.Join(" < ", frames)}");
            }
            _currentErrorMessage = value;
#if DEBUG || FAUNA_E2E_AGENT
            // `messages.error` is what `error_text()` reads with no wait, and the agent
            // otherwise publishes state only after a command or on its ~1 s idle push.
            // An error a machine tick raises after a bridge CLICK (no command at all —
            // the labeler catalog's refused subscribe) then read back as the stale
            // pre-click "" for up to a second after the page showed it. A change pushes
            // now, so the published mirror trails the screen by one POST, not one idle
            // tick.
            Testing.TestAgent.Instance?.RequestPush();
#endif
        }
    }

    // Return value of the most recent call_machine_method (the value-returning
    // bridge path, onboarding.md § E2E bridge contract § Return values). A parsed
    // System.Text.Json.JsonElement (or null), never the raw string — embedding a
    // string here would make SerializeState's own JSON re-encoding double-encode
    // it, the exact bug apple hit on this same field (onboarding.md, same §).
    // Written + cleared by TestAgent on the agent poll thread, read by
    // SerializeState on the thread pool — object is a reference type, so a plain
    // volatile field gives safe cross-thread visibility like _currentErrorMessage.
    private static volatile object? _machineMethodResult;
    internal static object? MachineMethodResult
    {
        get => _machineMethodResult;
        set => _machineMethodResult = value;
    }

    /// <summary>
    /// The target actor id (hex) for the NEXT profile-page navigation, or
    /// <c>null</c> for the viewer's own profile (SELF). Set just before navigating
    /// to the profile view — by the state-protocol nav handler for
    /// <c>{view:"profile", actor_id:&lt;hex&gt;}</c> (the e2e OTHER-profile path) and
    /// by the in-app tap-through (a ContactsPage row). <see cref="Views.ProfilePage"/>
    /// reads it in <c>OnNavigatedTo</c> and clears it, so a subsequent SELF nav
    /// (the sidebar <c>profile-tab</c>) sees <c>null</c>. Single-threaded UI nav, so
    /// no synchronization beyond the read-then-clear is needed.
    /// </summary>
    internal static string? PendingProfileTarget { get; set; }

    /// <summary>
    /// The actor's home nest a door carried beside <see cref="PendingProfileTarget"/>
    /// (the Find User result's <c>nest_url</c>, a post's / contact's
    /// <c>home_nest_url</c>, the state-protocol entry's <c>home_nest_url</c>) —
    /// read-then-cleared by <see cref="Views.ProfilePage"/> with the target and
    /// seeded into the profile's actor-home slot. <c>null</c> = no hint.
    /// </summary>
    internal static string? PendingProfileHome { get; set; }

    /// <summary>The mounted profile page's (actor id, actor-home slot), reported as
    /// the <c>data.profile</c> state read (<c>profile.actor_id</c> /
    /// <c>profile.home_nest_url</c>); <c>null</c> while no profile page is mounted.</summary>
    internal static (string ActorId, string? Home)? ProfileStateNow { get; set; }

    /// <summary>
    /// The <c>fauna://</c> route this process was launched with (argv), or
    /// <c>null</c> — the Explorer Share leaf's hand-off (windows.md § Shell
    /// Extension → <i>The Share hand-off</i>). A secondary launch forwards it to the
    /// running instance (<c>SingleInstanceManager.TryClaim</c>); a primary applies
    /// it once the main page is up (<see cref="ApplyPendingLaunchRoute"/>).
    /// </summary>
    internal static string? LaunchRoute { get; private set; }

    /// <summary>
    /// The file a <c>share-link</c> route asked for, consumed once by
    /// <see cref="Views.MediaPage"/> after its machine loads — the
    /// <see cref="PendingProfileTarget"/> read-then-clear shape.
    /// </summary>
    internal static (long FolderId, string Path)? PendingShareLink { get; set; }

    /// <summary>
    /// The set a <c>folder-share</c> route asked for, consumed once by
    /// <see cref="Views.FoldersPage"/> when it renders that set's row.
    /// </summary>
    internal static long? PendingFolderShare { get; set; }

    /// <summary>
    /// The request URI a <c>consent</c> route asked the Connected apps page to open,
    /// consumed once by <see cref="Views.ConnectedAppsPage"/> after its machine's
    /// first read (the <see cref="PendingShareLink"/> read-then-clear shape) and
    /// cleared only by <see cref="FinishConsentHandoff"/>, i.e. after the machine's
    /// <c>open_handoff</c> has returned.
    /// </summary>
    internal static string? PendingConsentHandoff { get; private set; }

    private static TaskCompletionSource? _consentHandoffOpened;

    /// <summary>
    /// Called by the Connected apps page once <c>open_handoff</c> has returned and the
    /// card (or the one expired-link message) is painted. Clears the staged request
    /// and releases whoever waits on <c>ApplyRouteAndWaitAsync</c> (the e2e twin, in
    /// Debug and e2e-agent builds).
    /// </summary>
    internal static void FinishConsentHandoff(string requestUri)
    {
        if (PendingConsentHandoff != requestUri) return;
        PendingConsentHandoff = null;
        _consentHandoffOpened?.TrySetResult();
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// The e2e seam's twin of <see cref="ApplyRoute"/>: applies the route, then waits
    /// for the route's page work — for a <c>consent</c> route, the page's
    /// <c>open_handoff</c> — so the card is painted when the caller continues.
    /// Signed out the route is only held, and nothing is awaited. UI thread.
    /// Compiled out of Release with the TestAgent, its one caller (convention 15).
    /// </summary>
    internal static Task ApplyRouteAndWaitAsync(string uri)
    {
        ApplyRoute(uri);
        return PendingConsentHandoff is null
            ? Task.CompletedTask
            : _consentHandoffOpened?.Task ?? Task.CompletedTask;
    }
#endif

    /// <summary>The first argv entry that is a <c>fauna://</c> URI, if any.</summary>
    private static string? RouteFromArgs(string[] args) =>
        args.Skip(1).FirstOrDefault(a => a.StartsWith("fauna://", StringComparison.OrdinalIgnoreCase));

    /// <summary>Apply the launch route once, now that the main page exists.</summary>
    internal static void ApplyPendingLaunchRoute()
    {
        if (LaunchRoute is not { } route) return;
        LaunchRoute = null;
        ApplyRoute(route);
    }

    /// <summary>
    /// Apply a <c>fauna://</c> route (windows.md § Shell Extension → <i>The Share
    /// hand-off</i>, step 4): stash the target for the destination page and navigate
    /// there. The grammar is shared Rust (<c>fauna_core::app_route</c>); a URI it does
    /// not parse is ignored, never an error. The share-link and folder-share arms
    /// mint, grant and write nothing; the consent arm makes the one nest call
    /// (<c>open_handoff</c>, on the page) and still grants nothing — approving is the
    /// user's tap on the card. Before sign-in there is no main page: the route then
    /// waits in <see cref="LaunchRoute"/>. UI thread.
    /// </summary>
    internal static void ApplyRoute(string uri)
    {
        var page = Views.MainPage.Current;
        if (page is null)
        {
            LaunchRoute = uri;
            return;
        }
        switch (uniffi.fauna_ffi.FaunaFfiMethods.ParseAppRoute(uri))
        {
            case uniffi.fauna_core.AppRoute.ShareLink link:
                PendingShareLink = (link.@folderId, link.@path);
                page.NavigateToView("media");
                break;
            case uniffi.fauna_core.AppRoute.FolderShare folder:
                PendingFolderShare = folder.@folderId;
                page.NavigateToSettingsSubPage(Core.Services.SettingsNavigation.Folders);
                break;
            case uniffi.fauna_core.AppRoute.Consent consent:
                // A newer route supersedes an older one still waiting for its page:
                // release the older waiter, it has nothing left to wait for.
                _consentHandoffOpened?.TrySetResult();
                _consentHandoffOpened = new TaskCompletionSource(
                    TaskCreationOptions.RunContinuationsAsynchronously);
                PendingConsentHandoff = consent.@requestUri;
                page.NavigateToSettingsSubPage(Core.Services.SettingsNavigation.ConnectedApps);
                break;
            default:
                Core.Logs.ShellLog.Warn("App", "ignored an unrecognized fauna:// route");
                break;
        }
    }

    /// <summary>
    /// Current inbox mode for state protocol — mirrors SettingsViewModel.InboxMode
    /// (SettingsPrivacyPage's PropertyChanged handler), the SAME property the page
    /// paints from, whether it changed via a user click (after the nest confirms
    /// the write) or SettingsViewModel.LoadAsync's own fetch on page load/relaunch.
    /// Empty ("", the seed default on both sides) means unknown — never seed a
    /// specific mode here, or a relaunch that has not yet re-fetched reports a
    /// mode the user never chose (settings.md item 7 — this used to seed "open" and never learn better on a fresh
    /// process, since only the click handler wrote it).
    /// </summary>
    internal static string TestInboxMode
    {
        get => _testInboxMode;
        set => _testInboxMode = value;
    }

#if DEBUG || FAUNA_E2E_AGENT
    // ── `barrier`'s self-test observables (e2e-conventions.md § convention 14) ──
    // Written on the UI thread (the DispatcherQueue items the probe enqueues, and
    // the barrier's own ack continuation), read by SerializeState from the agent
    // poll thread — volatile for the same reason `_currentErrorMessage` is.
    private static volatile string? _barrierProbe;
    private static volatile string? _barrierAckProbe;

    /// <summary>
    /// Apply one <c>barrier_probe</c> work item — the value the <c>i</c>-th queued
    /// item publishes (<c>fauna_e2e_agent::barrier_probe_value</c>, <c>"&lt;token&gt;#&lt;i&gt;"</c>).
    /// Called from the DispatcherQueue, i.e. the same UI-thread queue real deferred
    /// work rides and the queue position the barrier's own ack must land after.
    /// </summary>
    internal static void RecordBarrierProbe(string value) => _barrierProbe = value;

    /// <summary>
    /// Freeze what the barrier saw at its OWN ack
    /// (<c>fauna_e2e_agent::BARRIER_ACK_PROBE_KEY</c>), called from inside the
    /// barrier's UI-thread continuation — after every item enqueued before it ran.
    ///
    /// <para>⚠ The frozen copy is the whole point, and the live key is measurably
    /// vacuous: the driver reads state a round trip AFTER the ack, and this agent
    /// keeps republishing state on its poll loop meanwhile, so the queue has
    /// drained on its own by the time the read lands — a <c>barrier</c> that did
    /// nothing at all would pass. Never recompute this on a later publish.</para>
    /// </summary>
    internal static void FreezeBarrierAckProbe() => _barrierAckProbe = _barrierProbe;

    /// <summary>
    /// Drop both probe slots. A probe token is scoped to ONE test (tui's
    /// <c>App::barrier_probe</c> and linux's <c>clear_barrier_probe</c> have the
    /// same lifetime), so a leaked token would fire the next test's precondition
    /// assertion — which exists precisely so a stale value cannot make the
    /// self-test's real assertion vacuous.
    /// </summary>
    internal static void ClearBarrierProbes()
    {
        _barrierProbe = null;
        _barrierAckProbe = null;
    }
#endif

    // ── e2e nav-readiness barrier (Views.IAsyncLoadedPage) ──
    // A shell frame-nav arms this with the target page's LoadComplete task when that
    // page implements IAsyncLoadedPage; the TestAgent takes + awaits it (bounded) before
    // signalling ready=true, so `set_state({nav})` returns only once the page finished
    // its async Page_Loaded — not merely once the frame-nav was kicked off. Fixes the
    // whole-file test_folders.py wizard-render race (see Views.IAsyncLoadedPage). Only
    // touched under e2e (ArmNavLoad no-ops without a live TestAgent), so production
    // navigation is byte-unchanged. Single-threaded UI-thread arm-then-take — the plain
    // field needs no synchronization.
#if DEBUG || FAUNA_E2E_AGENT
    internal static Task? PendingNavLoad;
#endif

    /// <summary>Arm <see cref="PendingNavLoad"/> from <paramref name="content"/>'s
    /// <see cref="Views.IAsyncLoadedPage.LoadComplete"/> (null for a page that doesn't
    /// implement it, which clears any stale arm). Called by a shell right after its
    /// <c>Frame.Navigate</c>. No-op unless the e2e TestAgent is live.</summary>
    internal static void ArmNavLoad(object? content)
    {
        // Gated-real + no-op twin (testing.md convention 15): the method itself
        // always compiles, because SettingsShellPage calls it unconditionally on
        // every shell nav; only the body — which reads the Debug-only TestAgent —
        // is Debug-only. Production navigation was already byte-unchanged; now it
        // is byte-unchanged with no automation type referenced at all.
#if DEBUG || FAUNA_E2E_AGENT
        if (Testing.TestAgent.Instance is null) return;
        PendingNavLoad = (content as Views.IAsyncLoadedPage)?.LoadComplete;
#endif
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>Take + clear the armed nav-load task (the TestAgent awaits it once,
    /// bounded, before ready=true). Null when no IAsyncLoadedPage nav was armed.
    /// Debug-only: the TestAgent is its only caller.</summary>
    internal static Task? TakePendingNavLoad()
    {
        var t = PendingNavLoad;
        PendingNavLoad = null;
        return t;
    }
#endif

    public App()
    {
#if DEBUG || FAUNA_E2E_AGENT
        // `FAUNA_E2E_CULTURE` (e.g. "en-GB"), read before anything else runs —
        // `FaunaApp.Core.Calendar.WeekStart.Current` probes
        // `CultureInfo.CurrentCulture` fresh on every call, so this only needs
        // to land before the first page (the events page's week/month grids)
        // reads it, but doing it here, first, is simplest and covers every
        // future ambient-culture reader too. Unpackaged (`WindowsPackageType=
        // None`) apps have no per-process locale launch mechanism the way
        // apple's Foundation `-AppleLocale` argument-domain does — this env
        // var is windows' own e2e seam for the same class of test
        // (`test_events_locale_week_start_windows.py`). `DefaultThreadCurrentCulture` covers any thread
        // spawned after this point; `CurrentCulture` covers this one (the UI
        // thread every page later builds on). Compiled out of release builds
        // (convention 15: the automation surface never ships) — production
        // always takes the user's real Windows regional setting.
        var e2eCulture = global::System.Environment.GetEnvironmentVariable("FAUNA_E2E_CULTURE");
        if (!string.IsNullOrEmpty(e2eCulture))
        {
            try
            {
                var culture = new global::System.Globalization.CultureInfo(e2eCulture);
                global::System.Globalization.CultureInfo.CurrentCulture = culture;
                global::System.Globalization.CultureInfo.DefaultThreadCurrentCulture = culture;
            }
            catch (global::System.Globalization.CultureNotFoundException)
            {
                // Malformed test input; fall through on the machine's own culture
                // rather than crash a build that only exists to run tests.
            }
        }
#endif
        this.InitializeComponent();

        // The private contact overlay's one door reads over whichever manager the
        // app holds NOW (a thunk, so an actor-change swap is followed;
        // FaunaApp.Core.Services.ContactOverlayNames).
        FaunaApp.Core.Services.ContactOverlayNames.UseManager(() => FaunaApp.Conversations.ConversationsManagerHost.Instance);

        // A crash must SAY what it was. WinUI's XAML-generated handler only breaks
        // into an *attached* debugger (`App.g.i.cs`), so with none attached — which
        // is every e2e run and every real user — an unhandled exception kills this
        // process with 0xC000027B (STATUS_STOWED_EXCEPTION) having written nothing
        // anywhere: the Rust-side log holds only "logging initialised", and Windows
        // Error Reporting throttles the fault record away (measured 2026-08-31: the
        // crash below produced zero Application-log entries). The driver is left
        // with "App did not acknowledge <cmd>", which reads like a wedge and is not
        // one. A live provisioning run spent three paid Hetzner boxes on exactly
        // that silence.
        //
        // stderr is the channel that survives, because the e2e bridge starts this
        // app with UseShellExecute=false and no redirect of its own
        // (`flaui-bridge/SessionManager.cs::Launch`), so the app inherits the
        // bridge's handles and `drivers/windows.py::ack_timeout_diagnostics` quotes
        // that buffer straight back into the timeout message.
        //
        // It deliberately does NOT set `e.Handled` — swallowing a crash would trade
        // a diagnosable death for an undiagnosable zombie. The app still dies; it
        // just says why first.
        // ⚠ THREE hooks, not one, and the first is the one that does NOT fire for
        // the crash this was built for. `Application.UnhandledException` only sees
        // exceptions on the UI/XAML dispatcher thread. Measured 2026-08-31: with it
        // alone installed, a live provisioning run died with 0xC000027B and printed
        // NOTHING — because the throwing frame is on a thread-pool thread (the e2e
        // agent's own `TestAgent._backgroundAction` rail runs machine calls there,
        // and the app's async work generally does). An unhandled exception on a pool
        // thread tears the process down without ever reaching this event.
        this.UnhandledException += (_, e) => ReportFatal("UI thread", e.Exception, e.Message);

        // The catch-all: fires for an unhandled exception on ANY thread, immediately
        // before the runtime terminates. It cannot prevent the death and is not
        // trying to — it only makes it speak.
        global::System.AppDomain.CurrentDomain.UnhandledException += (_, e) =>
            ReportFatal("AppDomain", e.ExceptionObject as global::System.Exception, null);

        // A faulted Task nobody awaited. Does not kill the process on modern .NET,
        // which is exactly why it is worth printing: it is otherwise silent, and a
        // dropped async failure upstream is a plausible cause of the state the app
        // then dies in.
        global::System.Threading.Tasks.TaskScheduler.UnobservedTaskException += (_, e) =>
        {
            ReportFatal("unobserved Task", e.Exception, null);
            e.SetObserved();
        };

#if DEBUG || FAUNA_E2E_AGENT
        // ⚠ FOURTH hook, and the only one that sees the crash the other three miss.
        //
        // All three above report from a TERMINAL event — the runtime has already
        // decided to die and is telling us on the way out. An exception raised
        // inside a WinRT/XAML callback does not go out that way: the WinRT ABI
        // "stows" it and fails the process fast with 0xC000027B, and NONE of the
        // three fire. Measured 2026-09-01: FaunaApp died 0xC000027B in
        // Microsoft.UI.Xaml.dll on the Backups page, deterministically, and the
        // run captured not one `[fauna] FATAL` line —
        // which is exactly the silence the three hooks were added to end.
        //
        // FirstChanceException fires at THROW time, on the throwing thread,
        // before any handler and long before the stow, so it is the only place
        // such an exception is still visible. The cost is that it also sees every
        // exception that IS handled — routine control flow — so it is gated to
        // test-capable builds (convention 15: the automation surface is compiled
        // out of release artifacts) and marked FIRST-CHANCE rather than FATAL,
        // because most of what it prints is not a failure at all. Read it only
        // when a run dies with nothing else to go on: the LAST first-chance line
        // before the death is the candidate.
        global::System.AppDomain.CurrentDomain.FirstChanceException += (_, e) =>
        {
            try
            {
                global::System.Console.Error.WriteLine(
                    $"[fauna] FIRST-CHANCE {e.Exception.GetType().FullName}: {e.Exception.Message}");
                global::System.Console.Error.WriteLine(
                    e.Exception.StackTrace ?? "(no stack)");
                global::System.Console.Error.Flush();
            }
            catch
            {
                // As ReportFatal: a failure to report must not replace the
                // failure being reported — and here it would also re-enter.
            }
        };
#endif

        // Initialize i18n string localizer before any ViewModel is created
        Core.Services.Strings.Initialize(new Services.ResourceLoaderLocalizer());
        // The region content plane, opened at launch AHEAD of the first fetch so a
        // held document binds from the first paint (region-blocking.md § How an app
        // obtains its region's policy → last-known-good). The one platform leaf.
        Services.RegionLeaf.OpenPlane();
        // The nest-rules book, its persisted records restored (each verified again in
        // shared Rust) AHEAD of the first read, so the last-accepted rules bind from
        // the first paint (nest-rules.md § The nest content policy → Carriage).
        Core.Services.NestRulesHost.Open();
    }

    /// <summary>
    /// Write a fatal exception to stderr, the one channel that outlives this
    /// process. Never throws: a failure to report must not replace the failure
    /// being reported.
    /// </summary>
    /// <remarks>
    /// stderr specifically, not the Rust-side log: the e2e bridge starts this app
    /// with <c>UseShellExecute=false</c> and no redirect of its own
    /// (<c>flaui-bridge/SessionManager.cs::Launch</c>), so the app inherits the
    /// bridge's handles and <c>drivers/windows.py::ack_timeout_diagnostics</c>
    /// quotes the buffer back into the ack-timeout message. The app log is the
    /// wrong channel twice over — it held only "logging initialised" across three
    /// paid live runs, and a driver relaunch reopens it in mode <c>"w"</c>, which
    /// truncates the very lines naming the teardown.
    /// </remarks>
    private static void ReportFatal(string origin, global::System.Exception? ex, string? message)
    {
        try
        {
            global::System.Console.Error.WriteLine(
                $"[fauna] FATAL ({origin}) — {message ?? ex?.Message ?? "(no message)"}");
            global::System.Console.Error.WriteLine(
                ex?.ToString() ?? "(no exception object)");
            // AND the reporting thread's own live stack. Not redundant with the
            // line above: the one crash class this exists for arrives as a bare
            // `COMException (0x8000FFFF): Catastrophic failure` carrying NO stack
            // at all — XAML itself failing in native code, where no managed frame
            // ever threw and so `ex.StackTrace` is empty and no
            // FirstChanceException fires either. Measured 2026-09-09 on Windows
            // (): three separate instruments reported the
            // death and not one named a frame of ours.
            //
            // This handler runs ON the failing thread with the dispatch still on
            // the stack, so `Environment.StackTrace` is the one place the app's
            // own frame is still visible. It costs a stack walk on a path that is
            // already terminal.
            global::System.Console.Error.WriteLine(
                "--- reporting thread's live stack (the exception above may carry "
                + "none of its own) ---");
            global::System.Console.Error.WriteLine(
                global::System.Environment.StackTrace);
            global::System.Console.Error.Flush();
        }
        catch
        {
            // Deliberately empty — see the summary.
        }
    }

    /// <summary>
    /// Become (or remain) <paramref name="actorIdHex"/>'s single instance, refusing
    /// terminally if another live process already serves it or if this process is
    /// bound to a different account (account-scoping.md § Concurrent instances).
    /// Returns <c>false</c> when the caller must stop — the caller returns, it does
    /// NOT fall back onto another account.
    ///
    /// <para>Safe to call on every launch AND every switch: the shared holder
    /// distinguishes a fresh acquire from a same-account reuse from a cross-account
    /// swap, so one call site covers all three.</para>
    ///
    /// <para><b>It is also where this process claims its per-account activation
    /// endpoint</b> (account-scoping.md § Concurrent instances → <i>The per-(OS
    /// login, account) raise channel</i>). Here rather than beside the app-wide
    /// listener, because this is the one funnel every session account resolves
    /// through — plain, bound, and switched alike — which is exactly the set the
    /// goal doc says must claim one ("every serving desktop instance — plain and
    /// bound alike"). A <i>degraded</i> acquire claims it too: that instance is
    /// still serving the account, so a raiser must still be able to reach it.</para>
    /// </summary>
    private static bool EnsureSessionInstance(string actorIdHex)
    {
        var outcome = Core.Services.SessionInstance.Become(actorIdHex);
        if (outcome.MayProceed)
        {
            SingleInstanceManager.ServeAccount(actorIdHex);
            return true;
        }
        RefuseLaunch($"for {actorIdHex}: {outcome.Reason}");
        return false;
    }

    /// <summary>
    /// The refusal surface — one of the two duties the goal doc assigns the platform
    /// (the other is logging the degrade). Terminal by contract: a refused instance
    /// never falls back onto a different account.
    ///
    /// <para>The <c>[launch-refused]</c> marker is the cross-app convention the
    /// e2e instance-guard helper greps for, so windows' refusals read the same way
    /// linux's and tui's do. Written to stderr as well as the ring, because a
    /// refusal happens before the log file's own surface is reachable.</para>
    /// </summary>
    private static void RefuseLaunch(string detail)
    {
        var line = $"[launch-refused] {detail} — exiting";
        Console.Error.WriteLine(line);
        try { ShellLog.Error("App", line); } catch { /* the ring may not be installed yet */ }
        Environment.Exit(1);
    }

    /// <summary>
    /// A bound launch that cannot become its account — "launch bound or refuse".
    /// </summary>
    private static void RefuseBoundLaunch(string bound, string detail)
        => RefuseLaunch($"for {bound}: {detail}");

    /// <summary>
    /// Did this launch collide with a live instance of the account it would open —
    /// i.e. must it offer the launch-collision chooser instead of authenticating
    /// (account-scoping.md § Concurrent instances)?
    ///
    /// <para>The decision itself is the pure, unit-tested
    /// <see cref="Core.Services.LaunchCollisionGate.CollidesWithALiveInstance"/>;
    /// this supplies its two inputs — the store-active account (what a plain launch
    /// binds to) and the shared display-only probe.</para>
    ///
    /// <para><b>Must be called before <see cref="SingleInstanceManager.TryClaim"/>.</b>
    /// The mutex signals the primary and exits this process, so a collision decided
    /// after it is never decided at all. Its own registry handle is opened and
    /// disposed here rather than hoisting the launch's registry up: the launch's
    /// registry is deliberately constructed after the orphan-vault cleanup, and
    /// re-ordering that for a probe would trade a real invariant for a stylistic one.</para>
    ///
    /// <para>Fails toward the <b>ordinary launch</b>, never toward a chooser. The
    /// arbiter is still the acquire in <see cref="EnsureSessionInstance"/>, so the
    /// worst case of a wrong "no" is the terminal refusal that shipped before this
    /// existed — whereas a wrong "yes" would strand a lone launch in a chooser
    /// listing an account nobody is holding.</para>
    /// </summary>
    private static bool DetectLaunchCollision(string? launchBinding)
    {
        // Cheap short-circuit before opening anything: a bound launch never probes
        // (the goal doc's ordering, and it keeps a wired launch off the credential
        // store on a path that cannot use it).
        if (launchBinding is not null)
        {
            return false;
        }
        try
        {
            using var probe = CredentialStore.Registry();
            var active = probe.Active();
            var collided = Core.Services.LaunchCollisionGate.CollidesWithALiveInstance(
                launchBinding, active, Core.Services.SessionInstance.IsServed);
            if (collided)
            {
                ShellLog.Info(
                    "App",
                    $"[launch-collision] {active} is already served by a live instance — offering the chooser");
            }
            return collided;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[launch-collision] probe failed ({ex.Message}) — launching normally");
            return false;
        }
    }

    /// <summary>
    /// Render the launch-collision chooser and wait for the user to resolve it.
    /// Returns the actor id this process is now <b>bound</b> to (the pick), so the
    /// caller falls straight into its existing bound branch — there is no third
    /// process and no second launch path to keep in step with the first.
    ///
    /// <para>Returns <c>null</c> when the collision evaporated between detection and
    /// render (the other instance exited, or a CLI override re-pointed the active
    /// account), in which case the caller proceeds with the ordinary launch. The
    /// two forwarding exits never return at all — they exit the process.</para>
    /// </summary>
    private async Task<string?> ShowLaunchInstanceChooserAsync(
        Frame rootFrame, uniffi.fauna_ffi.FfiAccountRegistry registry)
    {
        // Re-confirm on the live registry. Detection ran before the CLI overrides
        // seeded it and before the window existed; both the served account and the
        // offerable set are display-only reads that can have moved since.
        var active = registry.Active();
        if (!Core.Services.LaunchCollisionGate.CollidesWithALiveInstance(
                Core.Services.SessionInstance.LaunchBinding, active, Core.Services.SessionInstance.IsServed))
        {
            ShellLog.Info("App", "[launch-collision] cleared before render — launching normally");
            return null;
        }

        var entries = registry.List();
        var servedLabel = entries
            .Where(e => string.Equals(e.@actorId, active, StringComparison.OrdinalIgnoreCase))
            .Select(e => uniffi.fauna_ffi.FaunaFfiMethods.AccountDisplayLabel(e.@handle, e.@actorId))
            .FirstOrDefault()
            ?? uniffi.fauna_ffi.FaunaFfiMethods.AccountDisplayLabel(null, active!);

        var choices = Core.Services.LaunchCollisionGate.ChoosableAccounts(
            entries.Select(e => (e.@actorId, e.@handle)),
            Core.Services.SessionInstance.NotCurrentlyServed,
            uniffi.fauna_ffi.FaunaFfiMethods.AccountDisplayLabel);

        var picked = new TaskCompletionSource<string?>(TaskCreationOptions.RunContinuationsAsynchronously);

        var ctx = new Views.LaunchInstanceChooserContext(
            ServedLabel: servedLabel,
            Choices: choices,
            Pick: actorId =>
            {
                // The list was a DISPLAY-ONLY probe, so re-probe the one row the
                // user actually clicked: an account free at render can be taken
                // before the click lands, and that is precisely what
                // `account_taken` says. Still not arbitration — the acquire in
                // EnsureSessionInstance remains the arbiter, and a race lost
                // between here and there is terminal, exactly as it is for a
                // FAUNA_BOUND_ACCOUNT launch.
                if (Core.Services.SessionInstance.IsServed(actorId))
                {
                    ShellLog.Warn("App", $"[launch-collision] {actorId} was taken between render and click");
                    return false;
                }
                // Bind, and let the BOUND branch below do the gating: it reads the
                // re-auth flag fresh from the store and runs the switcher's own
                // native prompt + `BindAccountConfirmed` retry. Calling
                // `BindAccount` here as well would either double-prompt a flagged
                // account or fork a second gate that can drift from the first —
                // and it is the bound seam WITH re-auth that the ratified design
                // asks the pick to enter ("bind_account + the bound seam, re-auth
                // included"). One gate, and it is the shipped, e2e-proven one.
                //
                // Everything below in OnLaunched — the session material read, the
                // instance-lock acquire, the launch machine's persistence —
                // resolves through this binding from here on, because they all
                // read LaunchBinding rather than the environment.
                Core.Services.SessionInstance.BindLaunchTo(actorId);
                picked.TrySetResult(actorId);
                return true;
            },
            FocusExisting: () =>
            {
                // Target the SERVED account's per-account endpoint, not the app-wide
                // name: the app-wide one is owned only by a plain instance, so this
                // exit could never reach a bound sibling — the gap the raise channel
                // closes (account-scoping.md § Concurrent instances → The per-(OS
                // login, account) raise channel). The fork itself is shared Rust
                // (resolve_focus_existing over UniFFI), adapted by
                // LaunchCollisionGate.ResolveFocusExisting.
                var outcome = Core.Services.LaunchCollisionGate.ResolveFocusExisting(
                    active!,
                    SingleInstanceManager.TryRaiseAccountInstance,
                    Core.Services.SessionInstance.IsServed);

                switch (outcome)
                {
                    case uniffi.fauna_ffi.FfiFocusExistingOutcome.Raised:
                        ExitAfterCurrentEvent();
                        break;

                    case uniffi.fauna_ffi.FfiFocusExistingOutcome.NoLongerServed:
                        // The sibling exited between the collision and the click.
                        // Nothing to raise and nothing in the way, so resolve the
                        // chooser as "no pick" — the caller falls through to the
                        // ordinary plain launch, the same route the
                        // cleared-before-render check above takes.
                        ShellLog.Info(
                            "App",
                            $"[launch-collision] {active} is no longer served — continuing as a plain launch");
                        picked.TrySetResult(null);
                        break;

                    case uniffi.fauna_ffi.FfiFocusExistingOutcome.StillServedNoChannel:
                        // Alive but unreachable (a tui server, or a client from
                        // before this leg). The page says so; do NOT exit into
                        // nothing, and do NOT claim the account — acquire would
                        // refuse a moment later anyway.
                        ShellLog.Warn(
                            "App",
                            $"[launch-collision] {active} is still served but owns no reachable endpoint");
                        break;
                }
                return outcome;
            },
            AddAccount: () =>
            {
                if (!SingleInstanceManager.TryForwardAddAccount())
                {
                    return false;
                }
                ExitAfterCurrentEvent();
                return true;
            });

        // The window is shown here rather than at the end of OnLaunched: the
        // chooser is a blocking surface, and the ordinary ShowLaunchWindow() sits
        // after the launch routing this await suspends.
        ShowLaunchWindow();
        rootFrame.Navigate(typeof(Views.LaunchInstanceChooserPage), ctx);
        return await picked.Task;
    }

    /// <summary>
    /// Exit the process once the current UI event has finished unwinding, instead
    /// of synchronously from inside it.
    ///
    /// <para>The chooser's exiting buttons (focus-existing, add-account) run their
    /// side effect — the raise / add-account forward — synchronously, then end this
    /// process. Calling <see cref="Environment.Exit"/> straight from the click
    /// handler tears the process down <i>mid-event</i>, which a synchronous UI
    /// Automation client (the FlaUI e2e bridge) observes as
    /// <c>E_UNEXPECTED</c>/"Catastrophic failure": its <c>InvokePattern.Invoke()</c>
    /// is still blocked in the doomed process when it vanishes. Deferring the exit
    /// onto the dispatcher lets the click event return and the invoke complete
    /// cleanly first; the process still exits immediately after, and the forward
    /// already happened synchronously, so the hand-off is never lost.</para>
    ///
    /// <para>A best-effort fallback to a direct exit covers the impossible case of
    /// no dispatcher — the process must still end, or the chooser's exit becomes a
    /// no-op.</para>
    /// </summary>
    private void ExitAfterCurrentEvent()
    {
        var queued = _window?.DispatcherQueue?.TryEnqueue(() => Environment.Exit(0)) ?? false;
        if (!queued)
        {
            Environment.Exit(0);
        }
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>What a launch broken by <c>FAUNA_E2E_FAULT_AT_LAUNCH=exit</c> exits
    /// with: non-zero, and a code no real failure of this app reports.</summary>
    internal const int E2eFaultAtLaunchExitCode = 73;
#endif

    protected override async void OnLaunched(LaunchActivatedEventArgs args)
    {
        // A `fauna://` route on argv (the installer's protocol registration starts
        // `FaunaApp.exe "<uri>"` — the Explorer Share leaf's hand-off). Captured
        // before the single-instance claim, which forwards it to a running
        // primary; a primary applies it once the main page is up.
        LaunchRoute = RouteFromArgs(Environment.GetCommandLineArgs());

        // This process's launch binding — the account it runs as when it is NOT the
        // plain/primary instance (account-scoping.md § Concurrent instances). Read
        // once, here, and threaded through the whole launch: every read below that
        // used to mean "the active account" must mean "this session's account".
        // Re-resolved through the succession chain once bound (rider 2, below,
        // near the bound branch's own `ResolveLaunchBinding` call) — this initial
        // read is what the chooser/collision logic above the bound branch sees.
        //
        // Read through SessionInstance.LaunchBinding, never the environment: the
        // launch-collision chooser can bind this process AFTER launch, and a client
        // that re-read FAUNA_BOUND_ACCOUNT would silently ignore its own chooser.
        var launchBinding = Core.Services.SessionInstance.LaunchBinding;

        // Install the process-global tracing subscriber (in-memory fauna_log ring +
        // daily-rolling file under <dataDir>/logs/ + stderr) as the first thing the
        // app does, so every app tracing event lands in the Settings → Logs ring +
        // the on-disk file (observability.md § Surfaces). Idempotent; seeds one
        // lifecycle line so the page is never empty. Rooted at the same
        // %LocalAppData%\Fauna dir as the pin store. Best-effort — never block startup.
        //
        // ⚠ This must precede EVERY path below that can end the process — the
        // credential reads, the instance-lock acquire, the single-instance redirect
        // and the collision probe — not follow them: a refusal that happens before
        // the subscriber exists leaves its `[launch-refused]` line nowhere on disk,
        // and the app dies with its only explanation unwritten. (Its "first thing
        // the app does" claim used to be false for exactly that span; it is now
        // literally the first thing, ahead of the window itself.)
        try
        {
            uniffi.fauna_ffi.FaunaFfiMethods.InstallLogging(BackupPaths.DataDir);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[startup] logging install failed: {ex.Message}");
            // no ShellLog: the ring failed to install
        }

#if DEBUG || FAUNA_E2E_AGENT
        // The app's start broken on purpose: the forced faults of convention 6's
        // launch-timeout rider (`test_launch_timeout_forced_windows.py`). Past the log
        // install, so the launch has a record, and before the window. `exit` ends the
        // process (the forced FAULTED); `block` parks the UI thread for good before
        // the window (the forced blocked launch, which asks for no CPU and must read
        // STALLED, never STARVED). Compiled out of a release artifact (convention
        // 15), so it is artifact-set wiring, never a knob.
        switch (Environment.GetEnvironmentVariable("FAUNA_E2E_FAULT_AT_LAUNCH"))
        {
            case "exit":
                Environment.Exit(E2eFaultAtLaunchExitCode);
                break;
            case "block":
                System.Threading.Thread.Sleep(System.Threading.Timeout.Infinite);
                break;
        }
#endif

        // Before the collision probe — the launch law's first read of the registry —
        // whatever a sign-out cut short owes is finished: the sync agent dismissed,
        // the wipe, each recorded account's erase, the residue re-sweep — so this
        // launch never offers the chooser for an account being signed out, never
        // routes Online over it and never takes an instance lock for it
        // (account-scoping.md § Erasure follows scope → The sign-out record is on
        // all 7 apps, decision 3). Off the UI thread, before the window exists. A
        // refused finish whose record still owes its wipe FAILS CLOSED (decision 4):
        // no collision probe, no session material, no launch machine — the fresh
        // wizard at identity_choice painting the refusal line, whose Remove Again
        // re-runs the finish. The single-instance gate below still runs, so when
        // the window that refused it is the primary, the redirect raises it and
        // its own sign-out records again and finishes both.
        var launchFinish = await FinishAtLaunchAsync();
        var launchFailedClosed = launchFinish.FailsClosed;
        _launchSignOutResidue = launchFinish.Residue;

        // Did this plain launch collide with a live instance of the account it would
        // open (account-scoping.md § Concurrent instances → "the colliding
        // instance's surface")? Answered HERE, before the single-instance gate
        // below, and that ordering is load-bearing rather than stylistic: the mutex
        // signals the primary and exits this process, so a collision decided after
        // it would never be decided at all. linux states the same constraint for
        // GApplication's D-Bus uniqueness, which is the first of the three things
        // it warns chooser platforms about.
        var collided = !launchFailedClosed && DetectLaunchCollision(launchBinding);

        // Single-instance gate (apps/windows.md § App Lifecycle, principle 1):
        // if another instance is already running, signal it to surface its window
        // and exit THIS process — never spawn a second windowless FaunaApp (the bug
        // that accumulated 3 instances and locked the app binaries during an MSI
        // upgrade). Disabled under the E2E bridge (the harness runs concurrent
        // instances against different nests). Best-effort: a guard fault falls
        // through to starting normally.
        //
        // The (OS login, account) re-key does NOT delete this layer — it narrows
        // what it answers (account-scoping.md § Concurrent instances → the
        // platform-affordance rule). The mutex stays in force for every UNBOUND
        // launch, which is what preserves raise-on-relaunch for the ordinary user.
        // A BOUND launch always runs as its own process: it opts out here (linux
        // adds NON_UNIQUE at exactly this point), and the shared AccountInstanceLock
        // taken below — once the session account resolves — is its only guard. The
        // two layers cannot disagree, because they answer different questions: the
        // mutex redirects unbound relaunches to the existing window; the lock
        // refuses a second same-account session, and a refused bound launch gets the
        // terminal contract rather than a raise (there is no coherent window of
        // "the other instance" to raise for a binding).
        //
        // A COLLIDED launch also opts out here, for the same structural reason a
        // bound one does: it is about to offer the chooser, and the redirect would
        // exit it first. Its focus-existing button is that redirect, reached
        // explicitly — so the raise-on-relaunch UX is preserved, just behind a
        // deliberate click instead of an automatic one.
        if (launchBinding is null && !collided && !SingleInstanceManager.TryClaim())
        {
            Environment.Exit(0);
            return;
        }

        _window = new Window
        {
            Title = "Fauna"
        };
        MainWindow = _window;
        // Screen time's foreground signal (family-safety.md § Screen time):
        // "active" for the heartbeat is foregrounded AND not locked, and this
        // window is the app's only window, so its Activated state IS the
        // app's foreground state. Wired once, app-lifetime (unlike the
        // per-login MainPage timers) — the flag defaults true, so a heartbeat
        // tick before the first Activated event still counts as foregrounded.
        _window.Activated += (_, args) =>
            ScreenTimeCache.SetWindowActive(args.WindowActivationState != WindowActivationState.Deactivated);

        // Window sizing is DPI-AWARE. AppWindow.Resize takes PHYSICAL pixels, so a fixed
        // physical size renders too small on a high-DPI display: a 600px window at 200%
        // scale is only 300 effective (DIP) units wide, which collapses the Settings
        // shell's Left-pane NavigationView content region to zero (every settings
        // sub-page — mail, privacy, encryption, … — becomes unreachable). Sizing without
        // DPI compensation silently broke windows settings/mail e2e once the VM display
        // moved to 200%. So target a fixed DIP extent scaled by the window's DPI, and
        // never restore a persisted window below that minimum (self-heals: the Closed
        // handler re-saves the clamped physical size).
        const int MinWindowDip = 600;
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(_window);
#if DEBUG || FAUNA_E2E_AGENT
        _mainWindowHandle = hwnd;
#endif
        var windowId = Microsoft.UI.Win32Interop.GetWindowIdFromWindow(hwnd);
        var appWindow = Microsoft.UI.Windowing.AppWindow.GetFromWindowId(windowId);
        var dpiScale = GetDpiForWindow(hwnd) / 96.0;   // 96 = 100%, 192 = 200%, …
        int minPx = (int)Math.Round(MinWindowDip * dpiScale);
        appWindow.Resize(new Windows.Graphics.SizeInt32(minPx, minPx));

        // Restore window state
        try
        {
            var localSettings = Windows.Storage.ApplicationData.Current.LocalSettings;
            if (localSettings.Values.TryGetValue("WindowWidth", out var savedW) &&
                localSettings.Values.TryGetValue("WindowHeight", out var savedH))
            {
                // Persisted geometry is in physical pixels; clamp up to the DPI-scaled
                // minimum so a too-small saved window can't strand the settings shell.
                appWindow.Resize(new Windows.Graphics.SizeInt32(
                    Math.Max((int)savedW, minPx),
                    Math.Max((int)savedH, minPx)));
            }
            if (localSettings.Values.TryGetValue("WindowX", out var savedX) &&
                localSettings.Values.TryGetValue("WindowY", out var savedY))
            {
                appWindow.Move(new Windows.Graphics.PointInt32((int)savedX, (int)savedY));
            }
        }
        catch
        {
            // ApplicationData.Current may not be available for unpackaged WinUI apps
        }

        var rootFrame = new Frame();
        _window.Content = rootFrame;

        // The ONLY writer of _mainAppMounted (see its declaration for why it is
        // derived and not assigned at the login seam). Registered here, before any
        // routing runs, so the very first Navigate is observed — the --autostart
        // gate below reads the flag right after the awaited launch dispatch, and
        // `MainPage` is navigated to synchronously inside it (StartMainAppAsync).
        rootFrame.Navigated += (_, e) => _mainAppMounted = e.SourcePageType == typeof(MainPage);

        // The one credential store, and the multi-account registry over it. The
        // launch machine routes on the registry's ACTIVE account
        // (`FfiAccountRegistry.LaunchPersistence()` → the shared
        // `RegistryLaunchPersistence`), so there is no second persistence path
        // and no C#-side identity plumbing — the platform's only foreign seam is
        // the key/value `FfiSecretStore` (long-term-store.md § Multi-account
        // evolution → Shared seam; nest/common.md § CR-3).
        //
        // (The tracing subscriber is installed at the very top of OnLaunched — ahead
        // of the collision probe and the single-instance gate, both of which can end
        // the process. See the comment there.)
        // The served account's material, read through the registry on every access
        // (RegistrySessionAccount) — keyed on the identity this window has loaded,
        // which every login path below moves.
        var account = new RegistrySessionAccount(
            () => _cryptoService is { HasKey: true } served ? served.ActorIdHex : null);
        using var registry = CredentialStore.Registry();
        string? secretHex;
        string? nestUrl;

        // Parse command-line overrides. `--secret` / `--nest-url` seed the
        // registry below (that's how the e2e launch-routing tests seed state).
        // `--reset` wipes the whole credential namespace; the legacy
        // `--nest-port` / `--auth-token` args were dropped when launch routing
        // moved onto fauna-launch-machine (the machine drives nest discovery +
        // the bearer; the test harness uses `--nest-url`).
        var cmdArgs = Environment.GetCommandLineArgs();
        string? cliSecretOverride = null;
        string? cliNestUrlOverride = null;
        bool resetState = false;
        bool autoStartLaunch = false;
        for (int i = 0; i < cmdArgs.Length; i++)
        {
            if (cmdArgs[i] == "--reset")
            {
                resetState = true;
            }
            else if (cmdArgs[i] == "--autostart")
            {
                // Sign-in launch via the per-user Run key (AutoStartService wrote
                // it as `"<exe>" --autostart`): come up tray-resident (hidden)
                // when routing lands in the main app — see the Activate() gate
                // below. apps/windows.md § App Lifecycle → Auto-start at
                // sign-in. The e2e harness DOES pass it (since 2026-08-09): the
                // tray-residency case drives both arms of that gate through
                // `make_launch_harness(..., extra_launch_config={"args": …})`.
                autoStartLaunch = true;
            }
            else if (i < cmdArgs.Length - 1)
            {
                if (cmdArgs[i] == "--secret")
                {
                    cliSecretOverride = cmdArgs[i + 1];
                }
                else if (cmdArgs[i] == "--nest-url")
                {
                    cliNestUrlOverride = cmdArgs[i + 1];
                }
            }
        }

        // --reset: clear stored credentials (used by E2E tests for clean state).
        // Goes through the registry's `ClearAll`, which erases every account's
        // per-actor slots and the index (long-term-store.md § Cleanup contract),
        // so no non-active account's secret is stranded in Credential Manager,
        // referenced by nothing. Delete-only, so a crash mid-wipe cannot resurrect
        // the identity being erased. --reset still wins over any --secret /
        // --nest-url passed alongside it (both are nulled here, so the machine
        // routes to WizardAt(IdentityChoice)).
        if (resetState)
        {
            registry.ClearAll();
            cliSecretOverride = null;
            cliNestUrlOverride = null;
        }

        // ── Registry boot: seed CLI overrides ──
        //
        // 1. The registry is the ONLY store (long-term-store.md § Downgrade mirror +
        //    abandoned-append recovery, the 2026-09-24 retirement): there is no
        //    pre-registry single slot to migrate from or mirror into, so boot runs
        //    no migration and no mirror. Launch routes on the registry alone, and
        //    every identity read below resolves the session account's material.
        //
        // 2. CLI overrides seed the registry rather than shadowing it, so the store
        //    stays the single source of truth the machine reads. `AddAccount` only
        //    takes `active` when nothing is active yet, hence the explicit
        //    `SetActive`: a box that already holds accounts must still switch to
        //    the identity the harness asked for. A malformed --secret is reported
        //    and ignored (the machine then routes on whatever was already active).
        if (cliSecretOverride is not null)
        {
            try
            {
                var cliActor = registry.AddAccount(cliSecretOverride, cliNestUrlOverride, null);
                registry.SetActive(cliActor);
            }
            catch (Exception ex)
            {
                ShellLog.Error("App", $"[startup] --secret override rejected: {ex.Message}");
            }
        }
        else if (cliNestUrlOverride is not null && registry.Active() is string activeActor)
        {
            // --nest-url alone re-points the active identity at the harness's nest.
            registry.SetNestUrl(activeActor, cliNestUrlOverride);
        }

        // 2b. The launch-collision chooser preempts everything below: this process
        //     cannot become the account the ordinary routing would resolve, so it
        //     must not read that account's credentials or run its silent challenge
        //     (account-scoping.md § Concurrent instances). Placed AFTER the CLI
        //     overrides so a harness-seeded launch still seeds the registry the
        //     chooser then reads, and BEFORE the session-material read so a
        //     collided launch never touches the active account's slots.
        //
        //     A pick returns the bound actor and falls through into the bound branch
        //     immediately below — the same branch a FAUNA_BOUND_ACCOUNT launch takes,
        //     which is what keeps the chooser from being a second launch path. The
        //     two forwarding exits never return (they exit the process).
        if (collided)
        {
            launchBinding = await ShowLaunchInstanceChooserAsync(rootFrame, registry);

            // No pick means the collision evaporated (cleared before render, or
            // focus-existing found the account no longer served), so this process
            // continues as an ORDINARY plain launch — and an ordinary plain launch
            // claims the app-wide single-instance name. Skipping it here was
            // harmless while the only way in was a rare race; focus-existing makes
            // it reachable by a click, and a primary that never claimed can never
            // be raised by a later relaunch. Redirect-and-exit if someone else
            // holds it, exactly as the ordinary path does.
            if (launchBinding is null && !SingleInstanceManager.TryClaim())
            {
                Environment.Exit(0);
                return;
            }
        }

        // 3. Resolve the session material this launch comes up with. A plain
        //    launch takes the ACTIVE account's — the account the launch machine
        //    below routes on — straight from the registry, the only store. After
        //    --reset (or on a fresh install) there is no active account and every
        //    local is null. A launch whose sign-out finish failed closed reads no
        //    account at all: the registry still names the accounts being signed
        //    out, and this launch must neither come up as one nor lock it.
        if (launchFailedClosed)
        {
            secretHex = null;
            nestUrl = null;
            _deviceId = null;
            _handle = null;
        }
        else if (launchBinding is null)
        {
            // Bare locals (still needed by StartMainAppAsync / SerializeState / the
            // test agent). The cached handle pre-populates settings/status views so
            // they render instantly on relaunch; the authoritative refresh comes
            // from the silent challenge's save_authenticated.
            var active = registry.Active() is string activeId ? registry.SessionMaterial(activeId) : null;
            secretHex = active?.@secretHex;
            nestUrl = active?.@nestUrl;
            _deviceId = active?.@deviceId;
            _handle = active?.@handle;
        }
        else
        {
            // A BOUND secondary instance resolves its session material through the
            // REGISTRY, for its own account (account-scoping.md § Concurrent
            // instances → "Session identity resolves through the session's account").
            //
            // ⚠ This is the blocker apple proved on a real bound launch, and it is
            // the one that makes a bound launch silently INERT rather than broken:
            // reading the ACTIVE account's material here would route the launch
            // machine on the bound account while the session came up as the active
            // one (apple observed am_i_admin=false for an admin binding). linux
            // records the same trap as the second of the three that bite chooser
            // platforms in order. One account-resolved accessor is what makes a
            // wrong-account read unrepresentable on session paths. A secondary
            // never moves `active` — the index stays read-mostly for it.
            //
            // Resolve through the succession chain BEFORE the gate below reads it
            // (account-scoping.md § Concurrent instances → "The binding follows the
            // account", rider 2): a bound launch whose named id has a recorded
            // successor in this install's registry binds to the TERMINAL successor,
            // never the retired id. `ResolveLaunchBinding` re-points the process
            // binding cell and hands back the binding as it stands right now — every
            // read below (this gate, `SessionMaterial`, `BoundLaunchPersistence`,
            // and `SessionInstance.LaunchBinding` itself) then names the successor.
            // Without this, a bound launch to a retired id resolves the retired id,
            // passes this gate, and only meets the nest's `superseded` refusal on
            // its first connect (mirrors tui `stored_account`/`launch_persistence`,
            // linux `session_binding` — both call the same shared rule at the same
            // point, as the registry is first in hand for the bound branch).
            launchBinding = registry.ResolveLaunchBinding() ?? launchBinding;
            // `bind_account` is the SINGLE gate on the binding — a pure read that
            // re-checks the account exists, has a stored secret, and is not
            // re-auth-flagged. The launch-wiring value is normalized but NOT
            // validated where it is read, precisely so a malformed one is refused
            // here as UnknownActor rather than quietly dropped.
            //
            // ConfirmationRequired is the one recoverable refusal: run the same
            // native re-auth the switcher runs, then retry through
            // BindAccountConfirmed. The flag is read FRESH from the store rather
            // than from a cached row — the admin auto-default can flip it on with no
            // refresh, and deciding from a stale false would skip the prompt (the
            // switcher's fail-closed gate, same reasoning).
            var flagged = registry.List()
                .FirstOrDefault(e => string.Equals(e.@actorId, launchBinding, StringComparison.OrdinalIgnoreCase))
                ?.@requireConfirmToActivate ?? false;
            try
            {
                if (flagged)
                {
                    // Fail-closed: a declined prompt refuses the launch. It never
                    // falls through to a plain launch, which would both put a second
                    // window on the active account and walk past the re-auth the
                    // flag demands.
                    if (!await Services.AccountReauth.ConfirmActivationAsync(_window))
                    {
                        RefuseBoundLaunch(launchBinding, "re-auth declined for a flagged account");
                        return;
                    }
                    registry.BindAccountConfirmed(launchBinding);
                }
                else
                {
                    registry.BindAccount(launchBinding);
                }
            }
            catch (Exception ex)
            {
                RefuseBoundLaunch(launchBinding, $"binding refused: {ex.Message}");
                return;
            }

            var material = registry.SessionMaterial(launchBinding);
            if (material is null)
            {
                // Bound-or-refuse: the binding names an account this install cannot
                // launch as (unknown, or no stored secret). Never fall back to a
                // plain launch — that would put a second window on the ACTIVE
                // account, which is exactly what the binding forbids.
                RefuseBoundLaunch(launchBinding, "no session material for the bound account");
                return;
            }
            secretHex = material.@secretHex;
            nestUrl = material.@nestUrl;
            _deviceId = material.@deviceId;
            _handle = material.@handle;
        }

        // Initialize crypto service and load secret if available
        _cryptoService = new CryptoService();
        if (secretHex is not null)
        {
            _cryptoService.LoadFromSecret(secretHex);
        }

        // Become this account's single instance (account-scoping.md § Concurrent
        // instances). THIS IS THE POINT the session account resolves — ActorIdHex
        // exists from the line above — and it is deliberately BEFORE any of the
        // account's scoped state opens: the MLS store, the feed drafts and the
        // backup-coordinator data dir all derive their paths from ActorIdHex, and
        // the whole point of the guard is that two processes never race one
        // account's scoped state. Acquiring after any of those opened would leave
        // the window the guard exists to close.
        if (_cryptoService.HasKey && !EnsureSessionInstance(_cryptoService.ActorIdHex))
        {
            return;
        }

        // Install the disk-backed nest-identity pin store before the first
        // authenticated connect, so TOFU pins (self-signed / LAN nests) survive
        // restarts (security.md § Transport trust). Rooted at the same
        // %LocalAppData%\Fauna dir the MLS store uses; the canonical pin filename
        // is appended inside Rust. Best-effort — never block startup.
        try
        {
            uniffi.fauna_ffi.FaunaFfiMethods.InstallNestIdentityPinStore(BackupPaths.DataDir);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[startup] nest-identity pin store install failed: {ex.Message}");
            ShellLog.Error("App", $"[startup] nest-identity pin store install failed: {ex.Message}");
        }

        // Initialize direct nest client (uses Ed25519 auth, no local proxy)
        var baseUrl = nestUrl ?? $"https://127.0.0.1:{DefaultNestPort}";
        // Dial the resolved (test-override-aware) URL; persist/render the literal
        // `baseUrl` everywhere else (onboarding.md § the dial seam) — resolving
        // `baseUrl` itself would leak a harness URL into SerializeState's
        // `node_url` and every other reader of the stored value, in release
        // builds, with no override installed
        // (dial_override_never_reaches_the_store.rs pins exactly this).
        var dialUrl = uniffi.fauna_launch_machine.FaunaLaunchMachineMethods.ResolvedDialUrl(baseUrl);
        _nestClient = new DirectNestClient(dialUrl, _cryptoService);
        _rpcClient = NewRpcClient(dialUrl, _cryptoService);

#if DEBUG || FAUNA_E2E_AGENT
        // Start test agent if E2E bridge URL is set — must happen before any
        // early returns so the agent is always active during tests.
        //
        // testing.md convention 15: this whole block, the agent it starts, and the
        // two closures it hands over (SerializeState / HandleTestCommand, both
        // Debug-only below) are absent from a Release build. The env-var read stays
        // the inner switch WITHIN a Debug build — convention 15's "runtime gates
        // stay" rule — so a debug build with FAUNA_E2E_BRIDGE unset behaves exactly
        // as before.
        var bridgeUrl = Environment.GetEnvironmentVariable("FAUNA_E2E_BRIDGE");
        if (bridgeUrl != null)
        {
            var agent = Testing.TestAgent.Start(bridgeUrl);
            agent.Configure(
                stateProvider: () => SerializeState(secretHex, nestUrl, account),
                commandHandler: (cmd) => HandleTestCommand(cmd, account, rootFrame, nestUrl),
                dispatcherQueue: _window.DispatcherQueue
            );
            // `painted_errors`' feed (fauna_e2e_agent::PAINTED_ERRORS_KEY): read
            // every painted frame's error surfaces from the app's own tree.
            Testing.PaintedErrorObserver.Install(_window, rootFrame);
            // `painted_text_area`'s feed (convention 17's `painted-text-has-area`):
            // every painted, id-bearing text element laid out with no area.
            Testing.PaintedTextAreaObserver.Install(_window, rootFrame);
        }
#endif

        // ── Launch routing via fauna-launch-machine ──
        //
        // Two "pending-X, no nest_url" cases (2a and 2) are NOT machine
        // outputs — the launch machine only knows about the saved pending
        // *invite*, and it never sees a nest_url for them. They're handled
        // here as C# pre-checks (identical to the pre-migration flow), before
        // the machine is constructed. Everything else goes through
        // LaunchMachine.Start() → a LaunchSnapshot.Phase dispatch:
        //
        //   - Online                       → main app (StartMainAppAsync)
        //   - WizardAt(IdentityChoice)      → wizard, cold start (no seed)
        //   - WizardAt(HandleEntry)         → wizard, identity seeded
        //   - WizardAt(InviteRequest)       → wizard, identity seeded +
        //                                     NavigateToInviteRequestForKnownNest
        //   - WizardAt(ClaimCode)           → wizard, identity seeded +
        //                                     NavigateToClaimCodeForKnownNest
        //   - Offline { transient }         → LaunchRetryPage (retry / use-a-
        //                                     different-nest)
        //
        // See docs/goal/behavior/onboarding.md § App-launch routing (migration
        // design tracked internally).

        // Post-Done orchestrator for the wizard. When the user finishes
        // onboarding, OnboardingPage invokes this callback so the launch
        // flow can pick up the freshly-saved credentials, re-run the launch
        // machine over the now-populated store, and navigate to the main app.
        // Declared via a separate "self" reference so the dispatch call inside
        // the body can pass the same callback (definite-assignment can't see
        // through the nested async lambda otherwise).
        System.Action onOnboardingCompleted = null!;
        onOnboardingCompleted = () =>
        {
            // T0 (onboarding-enable-email-checkbox + the sibling
            // onboarding-enable-{caldav,carddav,webdav}-checkbox): capture the
            // admin's enable-email AND enable-CalDAV/CardDAV/WebDAV intents from
            // the just-completed wizard machine BEFORE the handoff navigates away.
            // The wizard runs pre-identity but
            // fauna.bridges.set_{mail,caldav,carddav,webdav}_enabled are
            // Admin-class, so the toggles are recorded as intent only and fired
            // below once the authenticated Admin session exists. CalDAV/CardDAV/
            // WebDAV each gate independently of email and of each other
            // (caldav-server.md / carddav-server.md / webdav-server.md §
            // Independent enablement). Mirrors Linux's email_enable_requested() /
            // caldav_enable_requested() capture in views/onboarding/mod.rs.
            // onboarding.md §3b.
            var enableEmail = OnboardingViewModel.Current?.EmailEnableRequested ?? false;
            var enableCaldav = OnboardingViewModel.Current?.CaldavEnableRequested ?? false;
            var enableCarddav = OnboardingViewModel.Current?.CarddavEnableRequested ?? false;
            var enableWebdav = OnboardingViewModel.Current?.WebdavEnableRequested ?? false;
            // Record what was CAPTURED, separately from what the fire below does
            // with it. These are two independently-failing steps that present
            // identically (nothing enabled), and telling them apart used to need a
            // rebuild: a null `Current` (the wizard torn down before this ran)
            // reads every intent `false` via the `?? false` above, which looks
            // exactly like a machine that derived them OFF. One line here makes
            // the derivation and the firing separately observable — the
            // lesson (a symptom read three times as a product bug because the
            // ack path was never instrumented) applied to this hand-off.
            ShellLog.Info("App",
                $"[onboarding] derived serving intents (vm={(OnboardingViewModel.Current is null ? "null" : "live")}): "
                + $"mail={enableEmail} caldav={enableCaldav} carddav={enableCarddav} webdav={enableWebdav}");
            // onboarding.md's 2026-08-27 ruling: the terminal reads the secret
            // from the MACHINE, never the store, which holds it only if moment
            // 1's confirm-arm write landed — a machine-only drive
            // (seed_identity, the paid live-provisioning e2e's path; headlessly
            // pinned by test_onboarding_logged_in_terminal_empty_store.py) never
            // runs that arm, leaving the slot empty by construction. Read here,
            // alongside the intents above, before OnLoggedIn tears
            // the machine down.
            var effectiveSecret = OnboardingViewModel.Current?.EffectiveSecret;
            // The LoggedIn outcome's home nest and the device id the terminal
            // resolved — latched by the VM before OnLoggedIn fired, for the
            // append arm below, which registers the identity itself.
            var loggedInNestUrl = OnboardingViewModel.Current?.LoggedInNestUrl;
            var loggedInDeviceId = OnboardingViewModel.Current?.LoggedInDeviceId;
            _ = _window.DispatcherQueue.TryEnqueue(async () =>
            {
                try
                {
                    // Both come from the machine's LoggedIn outcome (captured
                    // above, before OnLoggedIn tore the wizard down) — never from
                    // the store, which an append-mode run never wrote and a
                    // first-run wizard wrote only through moment 4 below.
                    var newSecret = effectiveSecret;
                    var newNestUrl = loggedInNestUrl;
                    if (string.IsNullOrEmpty(newSecret) || newNestUrl is null)
                    {
                        System.Diagnostics.Debug.WriteLine(
                            "[onOnboardingCompleted] wizard Done with no effective secret/nest_url — staying on OnboardingPage.");
                        ShellLog.Warn("App",
                            "[onOnboardingCompleted] wizard Done with no effective secret/nest_url — staying on OnboardingPage.");
                        return;
                    }
                    // APPEND MODE ("Add account" over a live session). The wizard
                    // wrote NOTHING for this identity (moment 1's shared
                    // confirm-identity moment is write-free in append mode, and
                    // moment 4 is exempt), so `newSecret` is not yet an account.
                    // Register it, then run the SAME switch path a row tap takes —
                    // linux deliberately does not do a second single-identity boot
                    // here, and neither do we (long-term-store.md § Multi-account
                    // evolution: the append switches to the newly-added account).
                    if (_appendingAccount)
                    {
                        _appendingAccount = false;
                        try
                        {
                            using var appendRegistry = CredentialStore.Registry();
                            var addedActor = appendRegistry.AddAccount(
                                newSecret, newNestUrl, loggedInDeviceId);
                            // Dispose before the switch: SwitchAccountHandler opens its
                            // own registry view over the same underlying store.
                            appendRegistry.Dispose();
                            if (SwitchAccountHandler is not null)
                            {
                                // A just-added account is never re-auth-flagged (the
                                // flag is only ever set later, from its switcher row), so
                                // the confirmed switch path never applies here.
                                await SwitchAccountHandler(addedActor, false);
                            }
                        }
                        catch (Exception ex)
                        {
                            // Never swallowed: a failed append that silently returned
                            // to the old session is indistinguishable from a dropped
                            // click to the next reader.
                            ShellLog.Error("App", $"[add-account] append failed: {ex.Message}");
                            _currentErrorMessage = Core.Services.Strings.Error(ex);
                        }
                        return;
                    }

                    _cryptoService = new CryptoService();
                    _cryptoService.LoadFromSecret(newSecret);
                    // Become the just-onboarded account's single instance — the
                    // same entry point launch and the switch use, and for the same
                    // reason (account-scoping.md § Concurrent instances). Without it
                    // the process holder still names whatever identity this process
                    // served before the wizard (a relaunch's provisional identity
                    // that moment 1 has since retired, or a signed-out account), so
                    // `SessionAccount()` — what the switcher's "in use" row keys on —
                    // names an account that is no longer this window's.
                    if (_cryptoService.HasKey && !EnsureSessionInstance(_cryptoService.ActorIdHex))
                    {
                        return;
                    }
                    // Fresh machine over the registry. The VM's LoggedIn terminal
                    // has already run moment 4 — the shared PersistLoggedIn, which
                    // registered this identity's home nest PER-ACTOR and activated
                    // it (onboarding.md § Long-term store contract) — so the
                    // machine routes on it with no further write here, exactly as
                    // on the other six apps. Should land Online → StartMainAppAsync.
                    using var wizardRegistry = CredentialStore.Registry();
                    var wizardSelfAddressObserver = new Core.Services.SelfAddressHealObserver();
                    var machine = new LaunchMachine(
                        wizardSelfAddressObserver,
                        wizardRegistry.LaunchPersistence());
                    wizardSelfAddressObserver.Machine = machine;
                    _selfAddressHealObserver = wizardSelfAddressObserver;
                    await DriveLaunchAsync(
                        machine, machine.Start, rootFrame, account, newSecret, newNestUrl,
                        onOnboardingCompleted);
                    // Now that the authenticated session exists, hand the four
                    // intents to the ONE shared post-claim serving-enablement step
                    // (onboarding.md § 3b
                    // Mechanism — the same call tui, linux, web and apple make at
                    // their `LoggedIn` handoff; no enable step is fired from C#).
                    // The step is `am_i_admin`-discriminated (an admin claim
                    // honors the intents it derived; a new non-admin user
                    // auto-mints their own mailbox iff the deployment policy
                    // allows) and its non-admin branch must run even when all four
                    // intents are off (a new user never sees the wizard's
                    // derivation), so this is called unconditionally — the gates
                    // live inside the shared step. It also publishes the run's
                    // completion as the `serving_enablement` e2e state key.
                    // Fresh-onboarding-only (returning users take the
                    // launch-machine path below), so no once-per-login latch.
                    // mail-credentials.md § Auto-enable for new users.
                    //
                    // This step rides the session's own connection (_rpcClient,
                    // which the Online landing above built) — transport.md: one
                    // WebSocket per actor. A landing that did not reach Online has
                    // no session, so it is skipped rather than dialling a socket of
                    // its own.
                    var sessionRpc = _rpcClient;
                    if (sessionRpc is not null)
                    {
                        await ApplyPostClaimServingEnablementAsync(
                            sessionRpc, newNestUrl, enableEmail, enableCaldav, enableCarddav, enableWebdav);
                    }
                    else
                    {
                        ShellLog.Error("App",
                            "[onboarding] no session after the claim landing: post-claim serving enablement skipped");
                    }
                }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"[onOnboardingCompleted] handoff failed: {ex.Message}");
                    ShellLog.Error("App", $"[onOnboardingCompleted] handoff failed: {ex.Message}");
                }
            });
        };
        // Expose the hand-off to the e2e reset/logout test commands (HandleTestCommand)
        // so a post-reset onboarding run re-points the main client like a real launch.
        _onOnboardingCompleted = onOnboardingCompleted;

        // Factory-reset re-onboard hand-off (admin-factory-reset-button →
        // AdminSettingsPage). After fauna.admin.factory_reset returns the
        // post-reset claim code, tear down the authed session (keeping local
        // creds — the box was wiped, not the client) and re-seed onboarding at
        // claim_code with the code pre-filled. Reuses onOnboardingCompleted so
        // the re-claim + storage-mode re-pick completes back into the main app.
        // Mirrors linux's register_factory_reset_handler. The nest is mid-restart
        // (~1-2s); the claim-code page's transient-retry covers the WS drop.
        // Per docs/goal/behavior/mail-bridge-lifecycle.md § Factory reset.
        FactoryResetReonboardHandler = (nestUrl, handle, claimCode, secretHex) =>
        {
            // A factory reset drops the authenticated session too — counted
            // synchronously here, before the DispatcherQueue re-root below, for the
            // same reason as the switch and sign-out arms
            // (`Core.Services.E2eSessionCounters.RecordSessionTeardown`).
            Core.Services.E2eSessionCounters.RecordSessionTeardown();
            // The re-onboard supersedes a launch still in flight: its verdict is
            // dropped when it lands (Core.Services.LaunchCurrency). The re-claim's own
            // launch, built at the wizard's completion, is held afresh.
            Core.Services.LaunchCurrency.Shared.RetireInFlight();
            // A factory reset is a named actor-change boundary even though the
            // re-claim keeps this identity: the wiped nest invalidates whatever
            // every one of these surfaces had converged on (a standing critical
            // alert's condition, the memoized Bluesky machine, the guardian floor,
            // the conversations rails). ONE call, no list — DropActorScopedState.
            DropActorScopedState();
            // Session field, not process-lifetime state (same rule + reasoning as
            // the switch handler's identical line): DropActorScopedState resets
            // ConversationsManagerHost.Instance to a FRESH manager, so the observer
            // this field still points at is bound to the now-discarded one — null it
            // so StartMainAppAsync's re-entry knows to attach a new one to the new
            // manager rather than skip re-registration.
            _messageToastObserver = null;
            _ = _window.DispatcherQueue.TryEnqueue(() =>
            {
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient ?? new DirectNestClient(nestUrl, _cryptoService),
                    _cryptoService!, account,
                    SeedSecret: secretHex,
                    FactoryResetReonboard: (nestUrl, handle, claimCode),
                    OnOnboardingCompleted: onOnboardingCompleted));
            });
        };

        // Sign-out (Settings → Account sign-out-confirm-button). Local-only: wipe
        // the stored credentials so a restart cold-starts cleanly, then re-root
        // onboarding at identity_choice (no seed) — the cold-start nav shape from
        // DispatchLaunchSnapshotAsync's WizardAt(IdentityChoice) case. The
        // background pumps tied to the old authed session self-exit when the page
        // is replaced / on next identity load (see the ttl-loop note below).
        SignOutHandler = () =>
        {
            // The leaving identity — the account this window serves, the one its
            // instance lock names — read before anything below rebinds it.
            var leavingActor = _cryptoService is { HasKey: true } leavingCrypto ? leavingCrypto.ActorIdHex : null;
            // The sign-out's DECISION, on disk before anything else: after
            // SettingsAccountPage asked SignOutBlocked, before the teardown is
            // counted and before the first await. `sign-out-record.json` under the
            // install base names every account the registry lists plus this
            // window's, and owes the wipe, so a process killed anywhere after this
            // line — inside the runtime's stop, typically — is finished by the next
            // launch (account-scoping.md § Erasure follows scope → The sign-out
            // record is on all 7 apps, decision 1). The accounts it recorded ride to
            // this gesture's own finish below.
            var recorded = Core.Services.SignOutResidueSurface.RecordSignOut(SignOutResidueSeat, leavingActor);
            // A sign-out is a session teardown — counted synchronously here, at the
            // top of the deciding handler and before the DispatcherQueue re-root
            // below, for the same reason the switch arm counts where it does
            // (`Core.Services.E2eSessionCounters.RecordSessionTeardown`).
            Core.Services.E2eSessionCounters.RecordSessionTeardown();
            // A teardown supersedes a launch still in flight: its verdict is dropped
            // when it lands, never rendered over the wizard this re-roots to
            // (Core.Services.LaunchCurrency).
            Core.Services.LaunchCurrency.Shared.RetireInFlight();
            // The leave-flush, before DropActorScopedState drops the managers its
            // snapshots read; the erase below waits for it, because the erase retires
            // this machine's enrollment and the PUTs ride that session
            // (FlushDraftRailsAsync's own contract).
            var draftsFlushed = FlushDraftRailsAsync();
            // The leaving identity's push row is dropped on its own still-live
            // client before the erase.
            var leavingRpc = _rpcClient;
            var leavingDevice = account.ResolveDeviceId();
            // Sign-out is local-only (credentials wiped below), but nothing the
            // outgoing account converged on may survive into whatever account signs
            // in next. ONE call, no list — DropActorScopedState.
            DropActorScopedState();
            // Session field, not process-lifetime state — same reasoning as the
            // factory-reset arm's identical line just above in this
            // file.
            _messageToastObserver = null;

            if (!HydrationSessionEnabled)
            {
                StopSyncAgentSession();
            }

            // The wipe runs OFF the UI thread and the navigation is enqueued after
            // it, rather than both together on the dispatcher. `FinishSignOut`
            // waits for the account runtime to stop before erasing
            // (ReleaseAccountScopedStoresBeforeErase — an open store cannot be removed on
            // windows), and that wait on the UI thread would freeze the window for
            // its duration. Nothing in the wipe touches XAML: it is the credential
            // registry, the succession handoff and the on-disk stores. Only the
            // Navigate needs the dispatcher, and it must still run AFTER the wipe —
            // OnboardingPage reads the now-empty store to decide where the wizard
            // starts.
            _ = Task.Run(async () =>
            {
                // The draft PUTs started above land before anything below retires
                // the session they ride.
                await AwaitDraftFlushAsync(draftsFlushed);

                // Unprovision the out-of-app sync agent on the sign-out teardown path
                // (on-demand-files-apple.md § Multi-account × File Provider, consequence 1: every
                // out-of-app sync host is unprovisioned on every session-teardown
                // path), AWAITED before the erase — Core.Services.ErasePrecondition's
                // own doc explains why (gated like the provision path so an e2e run
                // without a real agent neither touches the box's installed
                // fauna-sync-agent.exe nor stalls on a dead-pipe connect).
                //
                // The erase is the one sign-out routine over the record written at
                // the top of this handler (FinishSignOut); what it left is RECORDED
                // before the wizard is built (install-scoped, so it outlives this
                // process) and handed to identity_choice's sign-out-residue view —
                // account-scoping.md § Erasure follows scope → the residue surface.
                // Push: the leaving identity drops its own row before the
                // credential erase — the last point its authority is in hand
                // (common.md § Push Notifications → *Registration*, the sign-out
                // leave-shape). Bounded and best-effort: a sign-out completes
                // offline. The install's opt-in bit survives, so the next sign-in
                // here re-arms.
                await Core.Services.PushSession.DropActorRowAsync(leavingRpc, leavingActor, leavingDevice);

                Core.Services.ISignOutResidueSurface? residue = null;
                await ErasePrecondition.AwaitUnprovisionThenErase(
                    HydrationSessionEnabled,
                    UnprovisionSyncAgentAsync,
                    () => residue = FinishSignOut(recorded));
                _ = _window.DispatcherQueue.TryEnqueue(() =>
                    rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                        _nestClient, _cryptoService, account,
                        OnOnboardingCompleted: onOnboardingCompleted,
                        SignOutResidue: residue)));
            });
        };

        // Steps 3b–5 of the account switch below, shared with the mid-session
        // escalation to the launch surface (EscalateToLaunchSurfaceHandler): count
        // the teardown, tear the outgoing session down, rebuild crypto + the
        // instance lock from the incoming material, and dispatch a fresh
        // LaunchMachine over the registry's active account. The escalation passes
        // the SAME account's material — the teardown is identical, the fresh
        // machine's challenge is what differs. Callers hold _switchPending.
        // `leaving` is a leave gesture's own last act on the outgoing session (the
        // switch's push-row drop), run past the teardown count and before anything is
        // torn down; the escalation, which keeps the same identity, passes none.
        async Task TearDownAndRelaunchAsync(
            uniffi.fauna_ffi.FfiAccountRegistry registry, string? newSecretHex, string? newNestUrl, string tag,
            Func<Task>? leaving = null)
        {
            // 3b. COUNT the teardown — here, and not one line lower.
            //     `fauna_e2e_agent::SESSION_GENERATION_KEY` asks for the
            //     INITIATION point: this is the first statement past every
            //     path that can still decline (the re-entrancy guard, the
            //     already-active return, the activation refusal above), and
            //     the last one before the first `await`. Counting after the
            //     await would put the bump behind the dispatcher round trip
            //     `barrier` performs, making the teardown invisible to the
            //     negative assert that reads this.
            Core.Services.E2eSessionCounters.RecordSessionTeardown();
            // The switch supersedes a launch still in flight from here, not only
            // once step 5 holds its own machine: the awaits between (the leave
            // gesture, the unprovision) are windows in which an older verdict would
            // otherwise land as current and render over this teardown
            // (Core.Services.LaunchCurrency).
            Core.Services.LaunchCurrency.Shared.RetireInFlight();

            // 3c. The torn-down shell is no longer an authenticated session, even
            //     though its MainPage stays on screen until the rebuild navigates.
            //     `session.authenticated` means "the authenticated main app is
            //     mounted" (e2e-conventions.md convention 11), and the Navigated
            //     hook alone would keep it true across the whole rebuild — while
            //     `session.actor_id` already names the INCOMING identity (the
            //     crypto swap below), so the state read "live as the successor"
            //     for the ~9 s LaunchMachine start before any session existed,
            //     and a caller's barrier on it passed mid-switch. The rebuild's
            //     own navigation to MainPage sets it again.
            _mainAppMounted = false;

            // 3d. The leave gesture's last act on the outgoing session, while its
            //     client is still live and its crypto still loaded. Bounded and
            //     best-effort by its own contract — never a gate on the teardown.
            if (leaving is not null)
            {
                await leaving();
            }

            // 4. TEAR DOWN the outgoing identity's session. DisposeNestClients
            //    alone is NOT enough — it drops the nest/RPC clients and the
            //    hydration loop, but leaves everything below bound to the
            //    signed-out identity.
            //
            // ⚠ Unprovision FIRST, before DisposeNestClients — the ordering is
            // load-bearing, and having it backwards made this a silent no-op
            // (found 2026-08-05 by test_sync_agent_unprovision_windows.py, whose
            // agent-log delta was empty). DisposeNestClients calls
            // StopSyncAgentSession, which drops the host's session; the awaited
            // UnprovisionSyncAgentAsync below then found nothing to unprovision and
            // returned a completed task, so the agent kept serving the SIGNED-OUT
            // account's on-demand engines under its still-valid capability —
            // exactly what on-demand-files-apple.md § Multi-account × File Provider,
            // consequence 1 forbids. Doing it first is safe: unprovision() stops
            // the convergence loop before it tears the capability down, so no tick
            // can re-provision from the still-loaded crypto and undo it (that
            // guarantee is why the old "stop it first" reasoning was unnecessary in
            // the first place). NOT ConfigureAwait(false): the continuation is
            // UI-bound, like this handler's other awaits. Best-effort; gated like
            // the provision path.
            if (HydrationSessionEnabled)
            {
                await UnprovisionSyncAgentAsync();
            }

            // Switch-shaped (the default): an account switch never erases the
            // outgoing identity's credential namespace, so its account-store
            // slot survives for a possible switch-back — retiring it here would
            // be wrong, per sync-agent-credentials.md § Credential model →
            // *The signed-out reconcile*'s rule.
            DisposeNestClients();

            // Everything the outgoing identity converged on — the standing
            // critical alerts, the memoized Bluesky machine, Guardian Notify's
            // counts + dedup set, the content/muted caches, the conversations
            // rails, the e2e snapshot, and the background loops still holding
            // the outgoing client. ONE call, no list — DropActorScopedState.
            // (DisposeNestClients above already reached it; idempotent by
            // design, and stating it here keeps the site readable without
            // making the drop a hidden side effect of disposing clients.)
            DropActorScopedState();

            // Session fields, not process-lifetime state: rebuilt by the
            // authenticated dispatch below, so they stay at this site per the
            // rule in DropActorScopedState's header.
            _messageToastObserver = null;
            _selfAddressHealObserver = null;
            ConvDrafts = null;
            FeedDraftsSync = null;
            FeedDrafts?.Dispose();
            FeedDrafts = null;
            // Unlike the two rails above, this rail's own sync handle is disposed
            // here too (identity seam — account-scoping.md § The scoping taxonomy;
            // EventDraftsService's own doc comment) rather than nulled only.
            EventDraftsSync = null;
            EventDrafts?.Dispose();
            EventDrafts = null;

            // Crypto is identity-bound — rebuild it from the INCOMING secret (the
            // post-wizard path does the same).
            _cryptoService = new CryptoService();
            if (newSecretHex is not null)
            {
                _cryptoService.LoadFromSecret(newSecretHex);
            }

            // 4b. SWAP the instance lock onto the incoming account, before step 5
            //     re-opens any scoped state under it (the MLS store, drafts and
            //     backup data dir all re-derive from the new ActorIdHex). Same
            //     entry point as launch: the shared holder acquires the incoming
            //     account's lock and releases the outgoing one by replacement, so
            //     a switch never leaves the departed account looking served.
            //     A refusal here is terminal for the same reason it is at launch —
            //     the incoming account is already live in another instance.
            if (_cryptoService.HasKey && !EnsureSessionInstance(_cryptoService.ActorIdHex))
            {
                return;
            }

            // 5. REBUILD: a fresh LaunchMachine over the registry's (now switched)
            //    active account, dispatched exactly as OnLaunched does. A switch
            //    TARGET can be a still-pending account (no nest_url yet — e.g. this
            //    exact handler, invoked from the pending-invite adoption seam
            //    below): the machine routes it to WizardAt(InviteRequest), and
            //    DispatchLaunchSnapshotAsync hydrates that arm from the target's
            //    own per-actor pending-invite slot — "what a relaunch would show"
            //    (onboarding.md § Multi-account).
            try
            {
                var switchSelfAddressObserver = new Core.Services.SelfAddressHealObserver();
                var switchMachine = new LaunchMachine(
                    switchSelfAddressObserver, registry.LaunchPersistence());
                switchSelfAddressObserver.Machine = switchMachine;
                _selfAddressHealObserver = switchSelfAddressObserver;
                // Held before its start: a launch still in flight (the cold one, a
                // Retry) is superseded by this one, and its verdict is dropped.
                await DriveLaunchAsync(
                    switchMachine, switchMachine.Start, rootFrame, account,
                    newSecretHex, newNestUrl ?? baseUrl, onOnboardingCompleted);
            }
            catch (Exception ex)
            {
                ShellLog.Error("App", $"[{tag}] machine start/dispatch failed: {ex.Message}");
                _currentErrorMessage = ex.Message;
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient, _cryptoService, account,
                    SeedSecret: newSecretHex,
                    OnOnboardingCompleted: onOnboardingCompleted));
            }
        }

        // Account switch (Settings → Account → an account-switcher-item row).
        // long-term-store.md § Multi-account evolution, design Decision 1: a switch
        // is `set_active` → teardown → rebuild, live in-session with NO relaunch.
        //
        // The ordering below is the load-bearing part, and it mirrors linux's
        // `register_switch_account_handler` step for step:
        //
        //   1. guard re-entrancy            (a double-fire tears down a rebuilding session)
        //   2. MUTATE  — SetActive, while the UI is intact
        //   3. READ    — the TARGET account's session material, by its actor id
        //   4. TEAR DOWN the outgoing identity's session
        //   5. REBUILD a fresh LaunchMachine and dispatch its snapshot
        //
        // Step 2-before-4 is a bug the fleet has already paid for once: mutating
        // first means a refusal (`NoStoredSecret` / `ConfirmationRequired`, both
        // enforced in the shared registry) aborts with the live session still
        // running. Step 3 names the target explicitly rather than asking "who is
        // in session": until step 4b swaps the instance lock, the session account
        // is still the OUTGOING one, and a read keyed on it would build the new
        // session from the old identity and never self-correct.
        SwitchAccountHandler = async (targetActorId, confirmed) =>
        {
            // 1. Re-entrancy guard. Interlocked, not a bool: this is invoked from the
            //    UI thread but awaits, so two clicks can interleave at an await point.
            if (System.Threading.Interlocked.CompareExchange(ref _switchPending, 1, 0) != 0)
            {
                ShellLog.Info("App", "[switch] ignoring re-entrant account switch");
                return;
            }
            try
            {
                using var switchRegistry = CredentialStore.Registry();

                // ⚠ Checked against the LIVE session (_cryptoService), NOT the
                // persisted registry's `active` pointer — measured bug: a caller that just wrote
                // `active` for an identity THIS PROCESS has never yet run as (the
                // append branch's AddAccount, or the identity-theft succession's
                // persisted arm — both mint/adopt a brand-new identity and hand it
                // straight to this same call) sees the store already agreeing with
                // its own write and this guard skipped the entire teardown+rebuild
                // — the outgoing session torn down nowhere, the incoming one never
                // launched, silently stranding the user with no session at all.
                // `_cryptoService` is this process's own answer to "who am I
                // actually serving right now", so it is the only source that can
                // tell "already active in the store" apart from "already live
                // here" — the switcher-row click this guard was built for keeps
                // its no-op, since a live row's account IS the loaded crypto.
                if (_cryptoService?.HasKey == true
                    && string.Equals(_cryptoService.ActorIdHex, targetActorId, StringComparison.Ordinal))
                {
                    return; // already on it — nothing to tear down
                }

                // 2. MUTATE FIRST, while the UI is still intact. The shared registry
                //    refuses an account it cannot launch as (NoStoredSecret) and a
                //    re-auth-flagged one activated WITHOUT confirmation
                //    (ConfirmationRequired) — both land here as exceptions with the live
                //    session untouched, which is exactly the point of doing this before
                //    any teardown.
                //
                //    `confirmed` is set by the VM only after the Windows Hello gate
                //    approved activating a flagged account (Stage 2, long-term-store.md
                //    § Per-account re-auth). SetActiveConfirmed is the ONLY post-re-auth
                //    activation path and this is its sole call site — the audit surface
                //    the goal doc requires to sit adjacent to the re-auth prompt. An
                //    unflagged switch uses plain SetActive; a flagged one whose prompt
                //    was skipped hits ConfirmationRequired here and fails loudly rather
                //    than silently bypassing the gate.
                try
                {
                    if (confirmed)
                    {
                        switchRegistry.SetActiveConfirmed(targetActorId);
                    }
                    else
                    {
                        switchRegistry.SetActive(targetActorId);
                    }
                }
                catch (Exception ex)
                {
                    // Surfaced, never swallowed: a dead click with no explanation is
                    // indistinguishable from a product bug to the next reader. Nothing has
                    // been torn down yet, so the refusal goes back to the caller that owns
                    // the surface — the Account page's switcher paints it on its
                    // `error-message` through Strings.Error, which shows shared Rust's
                    // `switch_refused_copy` sentence rather than the exception's aggregated
                    // `@msg=…` text (long-term-store.md § Multi-account evolution).
                    ShellLog.Error("App", $"[switch] activation refused: {ex.Message}");
                    throw;
                }

                // 3. READ the incoming identity's session material, keyed on the
                //    TARGET (see the step list above for why not the session account).
                var incoming = switchRegistry.SessionMaterial(targetActorId);
                var newSecretHex = incoming?.@secretHex;
                var newNestUrl = incoming?.@nestUrl;
                _deviceId = incoming?.@deviceId;
                _handle = incoming?.@handle;

                // The outgoing identity, read while it is still the one in session.
                var outgoingRpc = _rpcClient;
                var outgoingActor = _cryptoService is { HasKey: true } outgoingCrypto ? outgoingCrypto.ActorIdHex : null;
                var outgoingDevice = outgoingActor is null
                    ? null
                    : SessionDeviceId.Resolve(switchRegistry, CredentialStore.Logical, outgoingActor);

                // 3b–5: shared with EscalateToLaunchSurfaceHandler below. The switch
                // is committed (SetActive above), so the outgoing identity drops its
                // own push row on its still-live client before the teardown (common.md
                // § Push Notifications → *Registration*, the switch leave-shape); the
                // incoming identity re-arms at its session start.
                await TearDownAndRelaunchAsync(
                    switchRegistry, newSecretHex, newNestUrl, "switch",
                    leaving: () => Core.Services.PushSession.DropActorRowAsync(
                        outgoingRpc, outgoingActor, outgoingDevice));
            }
            finally
            {
                System.Threading.Volatile.Write(ref _switchPending, 0);
            }
        };

        // A mid-session session-ending verdict (security.md § Post-auth surfacing):
        // the switch's teardown + rebuild over the SAME active account, whose fresh
        // LaunchMachine earns the refusal again and lands launch routing's screen —
        // the import route for a supersession. Nothing to mutate first, so it is
        // steps 3b–5 alone. Under the switch's own guard: an escalation arriving
        // while a switch rebuilds (the stolen ceremony adopting its successor, say)
        // must not tear down the session that switch is building.
        EscalateToLaunchSurfaceHandler = async () =>
        {
            if (System.Threading.Interlocked.CompareExchange(ref _switchPending, 1, 0) != 0)
            {
                ShellLog.Info("App", "[escalate] ignoring: a switch or escalation is already rebuilding");
                return;
            }
            try
            {
                ShellLog.Warn("App", "[escalate] session ended — re-entering launch");
                using var escalateRegistry = CredentialStore.Registry();
                await TearDownAndRelaunchAsync(
                    escalateRegistry, account.SecretHex, account.NestUrl, "escalate");
            }
            finally
            {
                System.Threading.Volatile.Write(ref _switchPending, 0);
            }
        };

        // Stage 2 re-auth gate (long-term-store.md § Per-account re-auth): the switcher
        // VM consults this before activating a require_confirm_to_activate-flagged
        // account. Windows Hello via UserConsentVerifier, with the app window as the
        // consent-dialog owner; under e2e it reads the {FAUNA_E2E_CREDENTIAL_DIR}/
        // reauth-result file seam instead (Services.AccountReauth). Fail-closed.
        ConfirmReauthHandler = () => Services.AccountReauth.ConfirmActivationAsync(_window);

        // Append-mode adoption at the two deferred terminals (onboarding.md
        // § Multi-account, *Append-mode deferred/incomplete states*): the
        // pending-invite submit return ("the append glue adopts on the submit
        // return") and the AwaitingManualDns exit. Neither
        // reaches OnOnboardingCompleted above, which fires only on LoggedIn; the
        // VM reports each registry write here instead —
        // PersistPendingInviteSlot (windows twin of apple's
        // onPendingInvitePersisted) and HandleWizardOutcome's
        // PersistAwaitingDns — and an "Add account" wizard answers by leaving
        // append mode and switching to the identity the shared writer just
        // registered and activated. The switch's rebuild routes that account to
        // what a relaunch would show: invite_request, or "Almost ready".
        // _appendingAccount is cleared HERE, synchronously, before the switch task
        // starts — the one-shot latch: the VM fires this on every submit and on
        // every "Almost ready" recheck, but only the FIRST one (still in append
        // mode) may adopt. Clearing it before the switch also keeps the rebuilt
        // OnboardingPage (whose constructor reads App.IsAppendingAccount) from
        // coming up in append mode again, the way the Done-outcome terminal above
        // already clears it before its own switch.
        System.Action<string> adoptAppendedIdentity = actorId =>
        {
            if (!_appendingAccount) return;
            _appendingAccount = false;
            _ = _window.DispatcherQueue.TryEnqueue(async () =>
            {
                try
                {
                    if (SwitchAccountHandler is not null)
                    {
                        await SwitchAccountHandler(actorId, false);
                    }
                }
                catch (Exception ex)
                {
                    ShellLog.Error("App", $"[add-account] deferred-terminal switch failed: {ex.Message}");
                    _currentErrorMessage = Core.Services.Strings.Error(ex);
                }
            });
        };

        // "Add account" — append-mode onboarding over the LIVE session. Re-roots the
        // frame at the wizard with NO seed (a cold identity_choice start, so the user
        // can create or import), and latches _appendingAccount so the completion
        // closure appends + switches instead of booting a second single identity.
        //
        // The live session is deliberately left RUNNING behind the wizard: nothing is
        // torn down until the append actually succeeds and the switch path runs. That
        // is what makes abandoning it recoverable — see AbandonAddAccountHandler.
        AddAccountHandler = () =>
        {
            _appendingAccount = true;
            _ = _window.DispatcherQueue.TryEnqueue(() =>
            {
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient, _cryptoService, account,
                    OnOnboardingCompleted: onOnboardingCompleted,
                    OnPendingInvitePersisted: adoptAppendedIdentity,
                    OnAwaitingDnsPersisted: adoptAppendedIdentity));
            });
        };

        // Back out of an append. An append-mode wizard wrote nothing to the store
        // (moment 1 is write-free in append mode — long-term-store.md § Downgrade
        // mirror + abandoned-append recovery), so there is nothing to heal: we only
        // restore the running session's UI by re-dispatching the launch machine over
        // the (unchanged) active account — a pure re-render.
        AbandonAddAccountHandler = () =>
        {
            _appendingAccount = false;
            _ = _window.DispatcherQueue.TryEnqueue(async () =>
            {
                try
                {
                    using var abandonRegistry = CredentialStore.Registry();
                    var abandonSelfAddressObserver = new Core.Services.SelfAddressHealObserver();
                    var machine = new LaunchMachine(
                        abandonSelfAddressObserver, abandonRegistry.LaunchPersistence());
                    abandonSelfAddressObserver.Machine = machine;
                    _selfAddressHealObserver = abandonSelfAddressObserver;
                    await DriveLaunchAsync(
                        machine, machine.Start, rootFrame, account,
                        account.SecretHex, account.NestUrl ?? baseUrl,
                        onOnboardingCompleted);
                }
                catch (Exception ex)
                {
                    ShellLog.Error("App", $"[add-account] abandon restore failed: {ex.Message}");
                    _currentErrorMessage = Core.Services.Strings.Error(ex);
                }
            });
        };

        // Drive the launch machine — every routing row (the pending-invite resume
        // included) is its output. Wrapped defensively: a launch-time exception
        // (network, panic, FFI error) must not crash the app before the e2e bridge
        // connects — fall back to the retry surface (or onboarding-with-seed if
        // there's an identity).
        try
        {
            if (launchFailedClosed)
            {
                // The sign-out finish failed closed (above): no machine routes over
                // the registry. The fresh wizard, painting the refusal line; its
                // Remove Again re-runs the finish, and once that finish has wiped,
                // identity_choice is exactly where an empty registry routes.
                ShellLog.Warn("App",
                    "[launch] the sign-out finish failed closed — identity_choice with the refusal line, no launch machine");
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient, _cryptoService, account,
                    OnOnboardingCompleted: onOnboardingCompleted,
                    SignOutResidue: TakeLaunchSignOutResidue()));
            }
            else
            {
                // The CLI overrides were seeded into the registry at boot, so the
                // machine routes on the active account with no override plumbing.
                //
                // A BOUND instance routes on ITS account instead: the bound adapter
                // resolves the named account and never consults or moves `active`.
                // Pairing it with the session-material read
                // above is what makes a bound launch actually work — routing the
                // machine on the bound account while the session came up as the
                // active one is precisely the inert-bound-launch blocker.
                var persistence = launchBinding is null
                    ? registry.LaunchPersistence()
                    : registry.BoundLaunchPersistence(launchBinding);
                var selfAddressObserver = new Core.Services.SelfAddressHealObserver();
                var machine = new LaunchMachine(selfAddressObserver, persistence);
                selfAddressObserver.Machine = machine;
                _selfAddressHealObserver = selfAddressObserver;
                // launch_progress while the sign-in check runs: the account the app
                // is trying, from the registry's cache — blank on a first launch
                // (onboarding.md § App-launch routing → The cached identity on the
                // launch surfaces). The dispatch below navigates away from it.
                rootFrame.Navigate(typeof(Views.LaunchProgressPage), ReadLaunchIdentityLine());
                // The agent is already up (Testing.TestAgent.Start above), so an e2e
                // session patch can land while this start is in flight; a dropped
                // verdict falls through to the window decision below like a routed one,
                // and whatever superseded it navigates away from launch_progress.
                await DriveLaunchAsync(
                    machine, machine.Start, rootFrame, account, secretHex, baseUrl,
                    onOnboardingCompleted);
            }
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[launch] machine start/dispatch failed: {ex.Message}");
            ShellLog.Error("App", $"[launch] machine start/dispatch failed: {ex.Message}");
            var navParam = new ServiceClients(
                _nestClient, _cryptoService, account,
                SeedSecret: secretHex,
                OnOnboardingCompleted: onOnboardingCompleted,
                SignOutResidue: secretHex is null ? TakeLaunchSignOutResidue() : null);
            rootFrame.Navigate(typeof(OnboardingPage), navParam);
        }
        // The launch's residue paints on the cold launch's first identity_choice
        // or nowhere: a later landing (a switch, a sign-out) carries its own.
        _launchSignOutResidue = null;

        _window.Closed += (s, e) =>
        {
            try
            {
                var localSettings = Windows.Storage.ApplicationData.Current.LocalSettings;
                var pos = appWindow.Position;
                var size = appWindow.Size;
                localSettings.Values["WindowWidth"] = size.Width;
                localSettings.Values["WindowHeight"] = size.Height;
                localSettings.Values["WindowX"] = pos.X;
                localSettings.Values["WindowY"] = pos.Y;
            }
            catch
            {
                // ApplicationData.Current may not be available for unpackaged WinUI apps
            }
        };

        // Auto-start launches come up tray-resident: when the sign-in launch
        // (`--autostart`) routed into the main app, skip Activate() so no window
        // opens over a fresh desktop — everything residency needs (launch machine,
        // sync-agent spawn + capability provision, bearer TTL refresh, backup
        // driver, toast wiring) already ran above, code-driven, and the tray icon
        // below (double-click / Open) or any second launch (the single-instance
        // redirect) surfaces the window on demand. A --autostart launch that did
        // NOT reach the main app (no persisted session → onboarding) shows the
        // window: a signed-out auto-start must be loud, not a silently dead agent.
        // apps/windows.md § App Lifecycle → Auto-start at sign-in.
        bool showWindow = !(autoStartLaunch && _mainAppMounted);
        if (showWindow)
        {
            ShowLaunchWindow();
        }
#if DEBUG || FAUNA_E2E_AGENT
        // Publish the decision AFTER acting on it, never before: the e2e case
        // (tests/e2e-unified/tests/test_onboarding_launch_routing_smoke.py, case J)
        // uses the KEY'S PRESENCE as its causal barrier, so reading it must imply
        // Activate() has already been called or deliberately skipped. Publishing
        // first would hand the test a barrier that leads the fact it asserts.
        _launchAutostart = autoStartLaunch;
        _launchWindowShown = showWindow;
#endif
        TrayIconService.Initialize(_window);
        // Primary-only (no-op under E2E): listen for a second launch's activation
        // signal and surface this window — the single-instance redirect target.
        SingleInstanceManager.StartActivationListener(_window);
        // Restart-Manager cooperative shutdown (no-op under E2E): register for OS
        // restart + listen for session-end (WM_QUERYENDSESSION/WM_ENDSESSION) so an
        // MSI install/upgrade can close-and-relaunch us without force-killing, and
        // unsaved drafts are persisted on the way down. apps/windows.md
        // § App Lifecycle, "Cooperative shutdown for installers".
        RestartManagerService.Initialize();
    }

    /// <summary>
    /// One asynchronous launch drive, rendered only while it is still the launch this
    /// app holds (<c>onboarding.md</c> § App-launch routing → <i>A verdict renders only
    /// while its own launch is still the current one</i>; multiple-accounts outcome 17).
    /// <paramref name="drive"/> awaits <paramref name="machine"/>: a fresh machine's
    /// <c>Start()</c>, or a Retry/Trust re-send on the machine already on screen.
    ///
    /// <para>The machine is held in <see cref="Core.Services.LaunchCurrency"/> BEFORE the
    /// first await. A newer drive replaces it, and every superseding path clears it: the
    /// e2e session patch, sign-out and the factory-reset re-onboard. A verdict that lands
    /// after that is dropped whole, so nothing is rendered and nothing is torn down;
    /// whatever superseded it owns the routing. It is an ownership test, never an "is a
    /// session present?" one: the held launch's verdict still routes in full over a live
    /// session, which the <c>IdentityChanged</c> teardown depends on.</para>
    ///
    /// <para>A drive that THROWS has delivered its verdict too. It is counted like any
    /// other, and the caller's fallback renders it only while the launch is current.</para>
    /// </summary>
    private async Task DriveLaunchAsync(
        LaunchMachine machine,
        Func<Task> drive,
        Frame rootFrame,
        ISessionAccount account,
        string? secretHex,
        string baseUrl,
        System.Action onOnboardingCompleted)
    {
        var currency = Core.Services.LaunchCurrency.Shared;
        currency.Begin(machine);
        try
        {
            await drive();
        }
        catch (Exception ex)
        {
            if (currency.Deliver(machine)) throw;
            ShellLog.Info("App", $"[launch] a superseded launch failed ({ex.GetType().Name}) — dropping it");
            ForgetDroppedLaunch(machine);
            return;
        }
        if (!currency.Deliver(machine))
        {
            ForgetDroppedLaunch(machine);
            return;
        }
        await DispatchLaunchSnapshotAsync(
            machine, machine.Snapshot(), rootFrame, account, secretHex, baseUrl, onOnboardingCompleted);
    }

    /// <summary>
    /// A dropped launch takes its session field with it. The self-address observer built
    /// for its machine is what <see cref="CurrentLaunchIdentity"/> reads, so left in place
    /// it would answer for the newer session with the superseded launch's identity. A
    /// newer launch has already installed its own observer, so this clears only one that
    /// still belongs to the dropped machine (the e2e session patch builds none).
    /// </summary>
    private void ForgetDroppedLaunch(LaunchMachine machine)
    {
        if (_selfAddressHealObserver is { } observer && ReferenceEquals(observer.Machine, machine))
        {
            _selfAddressHealObserver = null;
        }
    }

    /// <summary>
    /// Routes a <see cref="LaunchSnapshot"/> (produced by
    /// <see cref="LaunchMachine.Start"/> or <see cref="LaunchMachine.RetrySilentChallenge"/>)
    /// to the appropriate next view. Phase mapping per
    /// <c>docs/goal/behavior/onboarding.md</c> § App-launch routing:
    ///
    /// <list type="bullet">
    /// <item><c>Online</c> → main app via <see cref="StartMainAppAsync"/>.
    ///   The persistence's <c>SaveAuthenticated</c> callback already wrote
    ///   handle/domain/tier to the vault's server-data cache.</item>
    /// <item><c>WizardAt(IdentityChoice)</c> → onboarding wizard, cold start
    ///   (no seed).</item>
    /// <item><c>WizardAt(HandleEntry)</c> → onboarding wizard, identity
    ///   seeded — the secret-only resume case.</item>
    /// <item><c>WizardAt(InviteRequest)</c> → onboarding wizard, identity
    ///   seeded + <c>NavigateToInviteRequestForKnownNest</c>. Reached when
    ///   /auth/verify returned 404 against a claimed (or unanswered-setup-
    ///   status) nest.</item>
    /// <item><c>WizardAt(AwaitingManualDns)</c> → onboarding wizard, identity
    ///   seeded + <c>SeedAwaitingManualDns</c>, rendering the "Almost ready"
    ///   surface. Reached when the persisted deferred-DNS slot is present
    ///   (onboarding.md § "Almost ready" surface) — checked before the
    ///   silent-challenge/ClaimCode rows since the nest is unreachable by
    ///   definition while DNS is pending.</item>
    /// <item><c>WizardAt(ClaimCode)</c> → onboarding wizard, identity seeded
    ///   + <c>NavigateToClaimCodeForKnownNest</c>. Reached when /auth/verify
    ///   returned 404 AND <c>setup-status.claimed == false</c>.</item>
    /// <item><c>WizardAt(PendingFactoryReset)</c> → onboarding wizard, identity
    ///   seeded + <c>FactoryResetReonboard</c> (claim_code pre-filled). Reached
    ///   when an admin dispatched <c>fauna.admin.factory_reset</c> and the
    ///   client died before the re-claim completed (gap CR-1,
    ///   nest/common.md § Client-state recoverability).</item>
    /// <item><c>Offline</c> with <c>accountIndexRefusal</c> set →
    ///   <see cref="Views.LaunchAccountIndexUnreadablePage"/>: the saved
    ///   account index is present and this build cannot use it
    ///   (version-compatibility.md § 5 item 9). Checked BEFORE the two
    ///   ordinary Offline arms below, since the machine projects it onto the
    ///   same terminal phase and carries the verdict on this side channel.
    ///   No retry, no "use a different nest" — the nest is not the
    ///   problem.</item>
    /// <item><c>Offline</c> with <c>lockedUntilSecs</c> set →
    ///   <see cref="Views.LaunchAccountLockedPage"/>: the account is locked
    ///   (devices.md § The locked state) — the standing notice and the
    ///   stolen-identity ceremony, no retry and no fallthrough; the surface
    ///   follows the machine's own refresh at the unlock time out
    ///   (<see cref="ShowLockedLaunch"/>). Checked ahead of the generic Offline
    ///   arms, like the account-index row.</item>
    /// <item><c>Offline { transient }</c> → <see cref="Views.LaunchRetryPage"/>:
    ///   Retry (re-runs the silent challenge) or "Use a different nest"
    ///   (drops to the wizard at handle_entry). The terminal
    ///   <c>Offline { transient: false }</c> case with no side channel is the
    ///   needs-update surface below.</item>
    /// <item><c>IdentityChanged</c> → <see cref="Views.LaunchIdentityChangedPage"/>:
    ///   the nest's pinned identity changed or can no longer be proven
    ///   (security.md § Transport trust). NO Retry CTA — only
    ///   "trust this nest" (forget the pin, re-TOFU, re-challenge) or "use a
    ///   different nest".</item>
    /// <item><c>default</c> (Boot/Hydrating/SilentChallenge/Refreshing —
    ///   shouldn't appear after <c>Start()</c> returns) → treated as a
    ///   transient <c>Offline</c>.</item>
    /// </list>
    /// </summary>
    private async Task DispatchLaunchSnapshotAsync(
        LaunchMachine machine,
        LaunchSnapshot snap,
        Frame rootFrame,
        ISessionAccount account,
        string? secretHex,
        string baseUrl,
        System.Action onOnboardingCompleted)
    {
        var cachedHandle = account.Handle ?? "";

        // A bound secondary instance NEVER enters the onboarding wizard
        // (account-scoping.md § Concurrent instances → "A secondary instance never
        // enters the onboarding wizard"). The wizard's moments write and activate
        // through the registry, which belongs to the primary; a bound launch that lands on a
        // wizard entry refuses and exits under the same terminal contract as a
        // refused binding — never a fallback to a plain launch, which would silently
        // drop the binding. The read-only blocking surfaces (Offline/retry,
        // NeedsUpdate, IdentityChanged) render normally when bound, so this gates
        // only WizardAt.
        //
        // Reads the PROCESS binding, not the environment: the chooser binds this
        // process after launch, and its pick must land under the same rule — a
        // chosen account whose slots route to onboarding has no wizard to run here
        // either.
        if (Core.Services.SessionInstance.LaunchBinding is { } boundActor
            && snap.@phase is LaunchPhase.WizardAt wizard)
        {
            RefuseBoundLaunch(
                boundActor,
                $"launch routed to wizard {wizard.@entry} — onboarding belongs to the primary instance");
            return;
        }

        // sync-agent.md § Credential model → *The signed-out reconcile*, shape (a)
        // (A10, row 130): this instance has no account and is about to render
        // onboarding, so nudge a reachable agent still serving THIS machine's
        // just-signed-out account to drop its capability now rather than wait on
        // its own renewal-loop cadence. Never reached from an append launch —
        // AddAccountHandler navigates straight to OnboardingPage, never through
        // this method — so no extra gate is needed here the way linux's shared
        // wizard builder needs one. Fire-and-forget, best-effort: every ambiguous
        // read (no local marker, unreachable agent, mismatched actor) is already a
        // no-op by the shared Rust's own design, so this is safe even on the rare
        // re-dispatch AbandonAddAccountHandler runs over the still-live active
        // account — that account was never the marker's subject, so the actor
        // match fails and nothing is torn down.
        if (snap.@phase is LaunchPhase.WizardAt)
        {
            _ = uniffi.fauna_ffi.FaunaFfiMethods.SignedOutOnboardingReconcile();
        }

        switch (snap.@phase)
        {
            case LaunchPhase.Online:
            {
                // The shared RegistryLaunchPersistence.save_authenticated already
                // wrote nest_url + handle/domain/tier onto the ACTIVE account —
                // StartMainAppAsync doesn't repeat the silent-sign-in refresh.
                var sh = secretHex ?? account.SecretHex;
                if (sh is null)
                {
                    // Defensive: machine says Online but we have no secret to
                    // init MLS / WS with. Shouldn't happen — fall back to the
                    // wizard at handle_entry.
                    System.Diagnostics.Debug.WriteLine(
                        "[launch] phase=Online but no secret available — falling back to wizard.");
                    ShellLog.Warn("App",
                        "[launch] phase=Online but no secret available — falling back to wizard.");
                    rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                        _nestClient, _cryptoService, account,
                        OnOnboardingCompleted: onOnboardingCompleted));
                    return;
                }
                await StartMainAppAsync(rootFrame, account, sh, baseUrl, machine);
                return;
            }

            case LaunchPhase.WizardAt w when w.@entry == LaunchWizardEntry.IdentityChoice:
            {
                // Cold start — no identity in the store. Wizard at IdentityChoice.
                //
                // A signed-out launch paints what the launch's own sign-out finish
                // left (FinishAtLaunchAsync, which ran before anything read the
                // registry: the record a dead sign-out left, then the residue
                // record's silent re-sweep) — the sign-out-residue view, only if
                // something is still left (account-scoping.md § Erasure follows
                // scope → The sign-out record is on all 7 apps, decision 3). Taken
                // once, so only the cold launch's first identity_choice paints it.
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient, _cryptoService, account,
                    OnOnboardingCompleted: onOnboardingCompleted,
                    SignOutResidue: TakeLaunchSignOutResidue()));
                return;
            }

            case LaunchPhase.WizardAt w when w.@entry == LaunchWizardEntry.HandleEntry:
            {
                // Identity present, no nest_url, no pending invite — the
                // secret-only resume case. Seed the identity; wizard at HandleEntry.
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient, _cryptoService, account,
                    SeedSecret: secretHex,
                    OnOnboardingCompleted: onOnboardingCompleted));
                return;
            }

            case LaunchPhase.WizardAt w when w.@entry == LaunchWizardEntry.InviteRequest:
            {
                // The pending-invite resume (onboarding.md § App-launch routing):
                // identity present, no nest_url, and a saved pending invite — the
                // machine routes it here off the account's per-actor slot. Seed
                // the slot so the wizard lands on invite_request with the snapshot
                // hydrated (nest_url/handle/request id/status_json) — "what a
                // relaunch would show", for a cold launch and for a switch onto a
                // still-pending account alike (onboarding.md § Multi-account).
                // Read back through the same store the machine branched on.
                if (account.NestUrl is null && account.PendingInvite is { } pendingInvite)
                {
                    rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                        _nestClient, _cryptoService, account,
                        SeedSecret: secretHex,
                        SeedPendingInvite: pendingInvite,
                        OnOnboardingCompleted: onOnboardingCompleted));
                    return;
                }
                // /auth/verify reported the secret isn't registered on the
                // saved nest (and setup-status reported claimed, or didn't
                // answer cleanly — safer default). Seed identity + nest_url
                // and land the wizard at invite_request so the user doesn't
                // re-type a handle they've already used.
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient, _cryptoService, account,
                    SeedSecret: secretHex,
                    NavigateToInviteRequestForKnownNest: (baseUrl, cachedHandle),
                    OnOnboardingCompleted: onOnboardingCompleted));
                return;
            }

            case LaunchPhase.WizardAt w when w.@entry == LaunchWizardEntry.AwaitingManualDns:
            {
                // Deferred-DNS resume (onboarding.md § "Almost ready" surface):
                // the user provisioned a nest, chose "Set up later" for DNS, and
                // quit before the claim landed. Checked before the silent-
                // challenge/ClaimCode rows — while DNS is pending the nest is
                // unreachable by definition, so challenging the saved nest_url
                // would only fall through to the retry surface.
                //
                // Read back through the same store the machine branched on, so
                // the record we seed is the record it routed on.
                var awaitingDns = account.AwaitingDns;
                if (awaitingDns is null)
                {
                    // The machine routed here off this very slot, so its absence
                    // now means the store changed underneath us. The identity
                    // survives, so fall back to the wizard rather than a blank
                    // "Almost ready" page with no records to add.
                    ShellLog.Warn("App",
                        "[launch] AwaitingManualDns row with no slot; falling back to the wizard.");
                    rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                        _nestClient, _cryptoService, account,
                        SeedSecret: secretHex,
                        OnOnboardingCompleted: onOnboardingCompleted));
                    return;
                }
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient ?? new DirectNestClient(awaitingDns.@nestUrl, _cryptoService!),
                    _cryptoService!, account,
                    SeedSecret: secretHex,
                    SeedAwaitingManualDns: (awaitingDns.@nestUrl, awaitingDns.@handle, awaitingDns.@dnsRecordsJson, awaitingDns.@claimCode),
                    OnOnboardingCompleted: onOnboardingCompleted));
                return;
            }

            case LaunchPhase.WizardAt w when w.@entry == LaunchWizardEntry.PendingFactoryReset:
            {
                // Factory-reset resume (gap CR-1, nest/common.md § Client-state
                // recoverability). The admin dispatched a factory reset and this client
                // died before the re-claim completed — possibly before the reply that
                // carried the claim code ever rendered. The code survives only because
                // it was minted and persisted BEFORE dispatch (AdminNestPage
                // .RunFactoryResetAsync), so seed the wizard's claim page from the slot:
                // the same surface as ClaimCode below, but pre-filled. The machine
                // checks this row before every other one — the box is wiped, so a
                // silent challenge could only fall through to the retry surface.
                //
                // Read back through the same store the machine branched on, so the
                // record we seed is the record it routed on. FactoryResetReonboard is
                // the existing ServiceClients seam the in-session reset already uses;
                // it drives NavigateToClaimCodeForKnownNestWithCode after SeedIdentity.
                var pending = account.PendingFactoryReset;
                if (pending is null)
                {
                    // The machine routed here off this very slot, so its absence now
                    // means the store changed underneath us. The identity survives, so
                    // fall back to the wizard rather than a claim page with no code.
                    ShellLog.Warn("App",
                        "[launch] PendingFactoryReset row with no slot; falling back to the wizard.");
                    rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                        _nestClient, _cryptoService, account,
                        SeedSecret: secretHex,
                        OnOnboardingCompleted: onOnboardingCompleted));
                    return;
                }
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient ?? new DirectNestClient(pending.@nestUrl, _cryptoService!),
                    _cryptoService!, account,
                    SeedSecret: secretHex,
                    FactoryResetReonboard: (pending.@nestUrl, pending.@handle, pending.@claimCode),
                    OnOnboardingCompleted: onOnboardingCompleted));
                return;
            }

            case LaunchPhase.WizardAt w when w.@entry == LaunchWizardEntry.ClaimCode:
            {
                // /auth/verify returned 404 AND setup-status reports
                // claimed=false — the saved nest is up but unclaimed, so the
                // user must claim it themselves. Seed identity + nest_url and
                // land the wizard at claim_code.
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient, _cryptoService, account,
                    SeedSecret: secretHex,
                    NavigateToClaimCodeForKnownNest: (baseUrl, cachedHandle),
                    OnOnboardingCompleted: onOnboardingCompleted));
                return;
            }

            // The saved account index is present and unreadable
            // (onboarding.md § App-launch routing — the row checked before
            // every other; version-compatibility.md § 5 item 9). Checked
            // BEFORE both ordinary Offline arms below, for the same reason
            // tui's route() checks it first: the machine deliberately
            // projects this to Offline { transient: false } and carries the
            // verdict on this side channel, so falling through to the
            // version-mismatch arm below would show "use a different nest" —
            // which misstates the problem, since the nest is fine.
            case LaunchPhase.Offline when snap.@accountIndexRefusal is { } refusal:
            {
                ShellLog.Warn("App", $"[launch] the saved account index is unreadable: {refusal}");
                var ctx = new Views.LaunchAccountIndexUnreadableContext(
                    Refusal: refusal,
                    OnConfirmStartOver: () =>
                    {
                        // The same routine a sign-out runs, behind the same record:
                        // the unreadable index names nobody, so the record owes the
                        // wipe with no accounts, and its finish sweeps every scope
                        // under the bases (account-scoping.md § Erasure follows scope
                        // → The sign-out record is on all 7 apps, decision 1). What
                        // it leaves is recorded and painted the same way — a
                        // start-over that could not remove everything must say so too.
                        var recorded = Core.Services.SignOutResidueSurface.RecordSignOut(SignOutResidueSeat, null);
                        var residue = FinishSignOut(recorded);
                        rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                            _nestClient, _cryptoService, account,
                            OnOnboardingCompleted: onOnboardingCompleted,
                            SignOutResidue: residue));
                    });
                rootFrame.Navigate(typeof(Views.LaunchAccountIndexUnreadablePage), ctx);
                return;
            }

            // The account is LOCKED (devices.md § The locked state; ui/sessions.md
            // § The locked surface): the machine parks here with the unlock time on
            // its side channel and runs the one refresh at that time itself. Ahead of
            // the generic split, which would paint a dead-end "update your nest" over
            // it — and of the superseded row, the order tui's route(), linux, android
            // and FaunaKit's dispatchLaunch take. The standing notice and its ONE
            // action, the stolen-identity ceremony; no retry, no fallthrough.
            case LaunchPhase.Offline when snap.@lockedUntilSecs is { } lockedUntil:
            {
                ShellLog.Warn("App", $"[launch] the account is locked until {lockedUntil}");
                ShowLockedLaunch(machine, lockedUntil, rootFrame, account, secretHex, baseUrl, onOnboardingCompleted);
                return;
            }

            // The identity was SUCCEEDED (identity-succession.md § Propagation →
            // *Own device fleet*): route to the import flow. Ahead of the generic
            // Offline split for the same side-channel reason as the account-index
            // row above — the machine projects the refusal to Offline { transient:
            // false }, so the version-mismatch arm would tell a succeeded user to
            // update their nest. Same position as apple's dispatchLaunch, tui's
            // launch.rs, linux's main.rs and web's onboarding page.
            case LaunchPhase.Offline when snap.@supersededSuccessor is { } claimed:
            {
                // That flow is onboarding, which belongs to the primary instance —
                // a bound launch refuses under the same contract as WizardAt.
                if (Core.Services.SessionInstance.LaunchBinding is { } boundSuperseded)
                {
                    RefuseBoundLaunch(
                        boundSuperseded,
                        "launch refused as superseded — importing the successor belongs to the primary instance");
                    return;
                }
                // The refused identity's secret + nest URL come from the ACTIVE
                // account's registry session material (`account` resolves through
                // the registry on every read), never a legacy mirror that can be empty.
                rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                    _nestClient, _cryptoService, account,
                    SupersededRefusal: (claimed, account.SecretHex ?? secretHex, account.NestUrl),
                    OnOnboardingCompleted: onOnboardingCompleted));
                return;
            }

            // The saved nest no longer signs this identity in (a claimed nest
            // answered the silent challenge not_registered — onboarding.md §
            // App-launch routing, the previously-signed-in row). The machine
            // projects it to Offline { transient: false } with the verdict on
            // the `sign_in_refused` side channel, so it is checked AHEAD of the
            // generic Offline arm below (which would paint the needs-update
            // surface: no Retry, and a misleading "update" framing). Same
            // position as linux's main.rs; Retry is the way back in once the
            // admin restores the account.
            case LaunchPhase.Offline when snap.@signInRefused:
            {
                ShellLog.Warn("App", $"[launch] phase=Offline(sign-in-refused): {snap.@lastError}");
                var ctx = new Views.LaunchSignInRefusedContext(
                    Message: snap.@lastError ?? string.Empty,
                    Retry: () => DriveLaunchAsync(
                        machine, machine.RetrySilentChallenge, rootFrame, account, secretHex, baseUrl,
                        onOnboardingCompleted),
                    Fallthrough: () => FallThroughToWizard(rootFrame, account, secretHex, onOnboardingCompleted));
                rootFrame.Navigate(typeof(Views.LaunchSignInRefusedPage), ctx);
                return;
            }

            case LaunchPhase.Offline o when o.@transient:
            {
                // Reachability failure (5xx, timeout, DNS, refused, …) the
                // client can't reliably classify, so offer Retry. snap.@lastError
                // carries the human-readable reason; LaunchRetryPage doesn't
                // surface it yet, so we keep its static "transient error" copy.
                // (The terminal `transient: false` case is the version-mismatch
                // arm below — it must NOT reach this retry path.)
                var lastError = snap.@lastError;
                if (lastError is not null)
                {
                    System.Diagnostics.Debug.WriteLine($"[launch] phase=Offline(transient): {lastError}");
                    ShellLog.Warn("App", $"[launch] phase=Offline(transient): {lastError}");
                }
                var ctx = new Views.LaunchRetryContext(
                    Retry: () => DriveLaunchAsync(
                        machine, machine.RetrySilentChallenge, rootFrame, account, secretHex, baseUrl,
                        onOnboardingCompleted),
                    Fallthrough: () => FallThroughToWizard(rootFrame, account, secretHex, onOnboardingCompleted),
                    // box-recovery.md § Recovery UI (step 4), surviving-device
                    // entry: RevealLaunchRecoverButtonAsync below already read
                    // this box list off the saved nest before the button became
                    // clickable — hand it straight to the recovery-seeded wizard.
                    Recover: (boxes) =>
                    {
                        if (secretHex is null) return;
                        rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                            _nestClient, _cryptoService, account,
                            RecoverFromLaunch: (secretHex, boxes),
                            OnOnboardingCompleted: onOnboardingCompleted));
                    },
                    Retire: () => OpenNestRetireFromLaunch(rootFrame, baseUrl, () =>
                        DispatchLaunchSnapshotAsync(
                            machine, machine.Snapshot(), rootFrame, account, secretHex, baseUrl,
                            onOnboardingCompleted)),
                    IdentityLine: ReadLaunchIdentityLine());
                rootFrame.Navigate(typeof(Views.LaunchRetryPage), ctx);
                // Best-effort, fired AFTER the retry surface paints (never
                // blocks it): a transiently-failing saved nest may still answer
                // a raw config read, or have recovered by the time this
                // resolves — box-recovery.md § Recovery UI (step 4).
                if (secretHex is not null && rootFrame.Content is Views.LaunchRetryPage retryPage)
                {
                    _ = RevealLaunchRecoverButtonAsync(retryPage, baseUrl, secretHex);
                }
                return;
            }

            case LaunchPhase.Offline:
            {
                // transient: false — the nest authoritatively reported it is
                // outdated (`fauna.nest.outdated` → degraded mode). Unlike the
                // transient arm above this is NOT an unreliable client guess: the
                // nest told us it cannot serve this client version, so retrying
                // the same nest is futile. Show a NON-retry "update required"
                // surface rendering the localized snap.@lastError in
                // `error-message`, no Retry button, keeping "Use a different
                // nest". version-compatibility.md Dim 4 / onboarding.md
                // § App-launch routing (version-mismatch row).
                var lastError = snap.@lastError;
                if (lastError is not null)
                {
                    System.Diagnostics.Debug.WriteLine($"[launch] phase=Offline(terminal/needs-update): {lastError}");
                    ShellLog.Warn("App", $"[launch] phase=Offline(terminal/needs-update): {lastError}");
                }
                var ctx = new Views.LaunchNeedsUpdateContext(
                    Message: lastError ?? string.Empty,
                    Fallthrough: () => FallThroughToWizard(rootFrame, account, secretHex, onOnboardingCompleted));
                rootFrame.Navigate(typeof(Views.LaunchNeedsUpdatePage), ctx);
                return;
            }

            case LaunchPhase.IdentityChanged changed:
            {
                // The nest's pinned deployment identity changed, or a pinned
                // nest can no longer prove any identity (security.md §
                // Transport trust — the SSH known_hosts model).
                // Auto-entry is BLOCKED and the bearer already dropped
                // machine-side. NO Retry CTA: a retry cannot change the
                // verdict and must never silently re-pin. The only ways out
                // are "trust this nest" (forget the pin, re-TOFU,
                // re-challenge — on THIS machine instance, which produced
                // the verdict and is the only one holding the secret +
                // nest_url off IdentityChanged state) and the fallthrough
                // (which must NOT forget the pin).
                //
                // COUNT the teardown — first statement in this arm, before any
                // await, same placement rule the sign-out/switch/factory-reset
                // sites use (`fauna_e2e_agent::SESSION_GENERATION_KEY` wants the
                // INITIATION point). A mid-session identity-changed escalation
                // is a session teardown exactly like those three — this arm was
                // the one gap: reachable only post-auth, with no bridge trigger
                // to exercise it until the silent_sign_in command existed to drive it.
                Core.Services.E2eSessionCounters.RecordSessionTeardown();
                ShellLog.Warn("App", snap.@identityFork
                    ? "[launch] phase=IdentityChanged: rotation-chain FORK evidence — two competing heads."
                    : "[launch] phase=IdentityChanged: possible nest identity change/withdrawal.");
                // Fork evidence (box-recovery.md § Client acceptance) is the snapshot's
                // `identity_fork` bit; the heads it names are the phase's own. The page
                // then paints the fork sentence and NO trust button.
                var ctx = new Views.LaunchIdentityChangedContext(
                    Trust: () => DriveLaunchAsync(
                        machine, machine.TrustNestIdentity, rootFrame, account, secretHex, baseUrl,
                        onOnboardingCompleted),
                    Fallthrough: () => FallThroughToWizard(rootFrame, account, secretHex, onOnboardingCompleted),
                    IsFork: snap.@identityFork,
                    PinnedHex: changed.@pinnedHex,
                    SeenHex: changed.@seenHex);
                rootFrame.Navigate(typeof(Views.LaunchIdentityChangedPage), ctx);
                return;
            }

            default:
            {
                // Boot / Hydrating / SilentChallenge / Refreshing — shouldn't
                // appear after `Start()` returns. Surface as a transient
                // retry so the user has a recovery path.
                System.Diagnostics.Debug.WriteLine(
                    $"[launch] unexpected phase after Start: {snap.@phase}");
                ShellLog.Warn("App",
                    $"[launch] unexpected phase after Start: {snap.@phase}");
                var ctx = new Views.LaunchRetryContext(
                    Retry: () => DriveLaunchAsync(
                        machine, machine.RetrySilentChallenge, rootFrame, account, secretHex, baseUrl,
                        onOnboardingCompleted),
                    Fallthrough: () => FallThroughToWizard(rootFrame, account, secretHex, onOnboardingCompleted),
                    Recover: (boxes) =>
                    {
                        if (secretHex is null) return;
                        rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
                            _nestClient, _cryptoService, account,
                            RecoverFromLaunch: (secretHex, boxes),
                            OnOnboardingCompleted: onOnboardingCompleted));
                    },
                    Retire: () => OpenNestRetireFromLaunch(rootFrame, baseUrl, () =>
                        DispatchLaunchSnapshotAsync(
                            machine, machine.Snapshot(), rootFrame, account, secretHex, baseUrl,
                            onOnboardingCompleted)),
                    IdentityLine: ReadLaunchIdentityLine());
                rootFrame.Navigate(typeof(Views.LaunchRetryPage), ctx);
                if (secretHex is not null && rootFrame.Content is Views.LaunchRetryPage retryPage)
                {
                    _ = RevealLaunchRecoverButtonAsync(retryPage, baseUrl, secretHex);
                }
                return;
            }
        }
    }

    /// <summary>
    /// "Use a different nest": walk the launch-target account away from its nest,
    /// keeping the identity, then seed the wizard at <c>handle_entry</c> with that
    /// identity — the one body every launch surface's fallthrough CTA runs (tui's
    /// <c>walk_away_from_nest</c>; <c>account-scoping.md</c> § Concurrent instances,
    /// the delete corollary).
    ///
    /// <para>The registry's <c>ClearNestBinding</c> deletes the account's
    /// (nest_url, device_id) slots and its reach hint and keeps the secret, so an
    /// abandoned re-onboard relaunches at the handle step instead of silently
    /// retrying the same nest (connect-and-sign-in outcome 16). The account is
    /// derived from <paramref name="secretHex"/> itself — the very seed the wizard
    /// receives — so the seed and the clear can never name two accounts, whichever
    /// dispatch path (plain launch, bound launch, switch, retry) handed the secret
    /// in. The identity-changed pin is not one of the slots the shared call
    /// deletes, so the fallthrough still never forgets it. Best-effort, as on
    /// tui/linux/android/web: a failed clear is logged and the seed goes ahead.</para>
    /// </summary>
    private void FallThroughToWizard(
        Frame rootFrame, ISessionAccount account, string? secretHex, System.Action onOnboardingCompleted)
    {
        if (secretHex is { Length: > 0 })
        {
            try
            {
                var actor = Convert.ToHexString(uniffi.fauna_ffi.FaunaFfiMethods.ActorIdFromSecret(
                    Convert.FromHexString(secretHex))).ToLowerInvariant();
                using var registry = CredentialStore.Registry();
                registry.ClearNestBinding(actor);
            }
            catch (Exception ex)
            {
                ShellLog.Warn("App",
                    $"[launch] use-a-different-nest could not clear the nest binding: {ex.GetType().Name}: {ex.Message}");
            }
        }
        rootFrame.Navigate(typeof(OnboardingPage), new ServiceClients(
            _nestClient, _cryptoService, account,
            SeedSecret: secretHex,
            OnOnboardingCompleted: onOnboardingCompleted));
    }

    /// <summary>The locked launch surface this process is showing (and following),
    /// or <c>null</c>. Compared by reference: a tick or a re-read owed to a surface
    /// that has since gone is dropped.</summary>
    private Core.ViewModels.LockedLaunchViewModel? _lockedLaunch;

    /// <summary>
    /// Land the locked launch surface (<c>launch_account_locked</c>) for
    /// <paramref name="lockedUntil"/>, and follow <paramref name="machine"/> out of
    /// it (<c>ui/sessions.md</c> § The locked surface; <c>devices.md</c> § The locked
    /// state). The same lock keeps its surface — and an open stolen-identity step;
    /// a new one gets a fresh one.
    ///
    /// <para><b>Following.</b> The machine runs the one refresh at the unlock time
    /// itself; on each of its ticks (the launch observer's <c>Ticked</c>) the shell
    /// re-reads the snapshot and re-dispatches once the shared rule
    /// (<c>launch_lock_notice_holds</c>, through
    /// <see cref="Core.ViewModels.LockedLaunchViewModel.NoticeHolds"/>) says the notice
    /// no longer stands — signed back in, or met a different lock. No timer of this
    /// app's own. FaunaKit's <c>followLockedLaunch</c> is the twin.</para>
    ///
    /// <para><b>The ceremony</b> rides no session: the shared session-less
    /// composition <c>succession_succeed_stolen_locked</c> over an anonymous
    /// connection, with the seed this device still holds as the old identity and the
    /// platform store location (no sandbox container on windows). A persisted landing
    /// adopts the successor through the ordinary account switch, which re-runs
    /// launch over it.</para>
    /// </summary>
    private void ShowLockedLaunch(
        LaunchMachine machine,
        ulong lockedUntil,
        Frame rootFrame,
        ISessionAccount account,
        string? secretHex,
        string baseUrl,
        System.Action onOnboardingCompleted)
    {
        if (_lockedLaunch is { } shown
            && shown.LockedUntilSecs == lockedUntil
            && rootFrame.Content is Views.LaunchAccountLockedPage)
        {
            return;
        }

        var heldSecret = account.SecretHex ?? secretHex;
        var nestUrl = account.NestUrl ?? baseUrl;
        string? predecessor = null;
        if (heldSecret is { Length: > 0 })
        {
            try
            {
                predecessor = Convert.ToHexString(uniffi.fauna_ffi.FaunaFfiMethods.ActorIdFromSecret(
                    Convert.FromHexString(heldSecret))).ToLowerInvariant();
            }
            catch (Exception ex)
            {
                ShellLog.Warn("App", $"[locked] the held secret names no actor: {ex.GetType().Name}");
            }
        }

        var model = new Core.ViewModels.LockedLaunchViewModel(
            lockedUntil,
            async kitInput =>
            {
                if (heldSecret is not { Length: > 0 })
                    throw new InvalidOperationException("this device holds no identity to recover");
                using var registry = CredentialStore.Registry();
                return await uniffi.fauna_ffi.FaunaFfiMethods.SuccessionSucceedStolenLocked(
                    nestUrl, Convert.FromHexString(heldSecret), kitInput, registry,
                    new Core.Services.SuccessorStorePath());
            },
            predecessor);
        _lockedLaunch = model;

        void Follow()
        {
            if (!ReferenceEquals(_lockedLaunch, model)) return;
            var snap = machine.Snapshot();
            if (model.NoticeHolds(snap)) return;
            var phaseName = DescribePhase(snap);
            ShellLog.Info("App", $"[locked] the lock no longer stands ({phaseName}) — re-dispatching");
            _lockedLaunch = null;
            _ = DispatchLaunchSnapshotAsync(
                machine, snap, rootFrame, account, secretHex, baseUrl, onOnboardingCompleted);
        }

        // The observer the app wired onto THIS machine when it built it; a machine
        // built without one cannot be followed, and says so.
        var observer = _selfAddressHealObserver;
        if (observer is not null && ReferenceEquals(observer.Machine, machine))
        {
            observer.Ticked = () => _window?.DispatcherQueue.TryEnqueue(Follow);
        }
        else
        {
            ShellLog.Warn("App", "[locked] no observer on this launch machine — the notice cannot follow its refresh");
        }

        var ctx = new Views.LaunchAccountLockedContext(
            Model: model,
            Switch: successor =>
                SwitchAccountHandler?.Invoke(successor, false) ?? Task.CompletedTask,
            Refollow: Follow,
            Left: () =>
            {
                if (!ReferenceEquals(_lockedLaunch, model)) return;
                _lockedLaunch = null;
                if (observer is not null && ReferenceEquals(observer.Machine, machine)) observer.Ticked = null;
            });
        rootFrame.Navigate(typeof(Views.LaunchAccountLockedPage), ctx);
        // A tick may have landed between the snapshot this dispatch read and the
        // hook going up; re-read once so it is not lost.
        Follow();
    }

    /// <summary>The launch phase's variant name, for a log line.</summary>
    private static string DescribePhase(LaunchSnapshot snapshot)
    {
        LaunchPhase phase = snapshot.phase;
        return phase.GetType().Name;
    }

    /// <summary>
    /// Run the deployment-seed custody leg (<c>DeploymentSeedCustody.SelfHealAsync</c>)
    /// over the session's connection and, when it answers a warning (custody
    /// unconfirmed — the shared Rust mapping chose which), render it through
    /// <c>Strings.Resolve</c> onto the landing page's WarningBar via the canonical
    /// message-bar setter (android's <c>AppMessages.showWarning</c> precedent).
    /// Continuations stay on the captured UI context (no <c>ConfigureAwait(false)</c>
    /// anywhere in the chain). A result for a session that has since been replaced
    /// (account switch, sign-out) is dropped, never shown on the new account.
    /// </summary>
    private async Task RunDeploymentSeedCustodyLegAsync(NestRpcClient? rpc)
    {
        if (rpc is null) return;
        var custody = await Core.Helpers.DeploymentSeedCustody.SelfHealAsync(rpc);
        if (custody is null || !ReferenceEquals(rpc, _rpcClient)) return;
        var custodyWarning = Core.Services.Strings.Resolve(custody);
        _currentWarningMessage = custodyWarning;
        Views.MainPage.Current?.UpdateTestMessages(
            _currentErrorMessage, custodyWarning, _currentInfoMessage);
    }

    /// <summary>
    /// box-recovery.md § Recovery UI (step 4), surviving-device entry:
    /// best-effort box-list read off the saved nest, fired
    /// AFTER <see cref="Views.LaunchRetryPage"/> has already painted (never
    /// blocks the retry surface — <see cref="Views.LaunchRetryPage.ShowRecoverButton"/>
    /// reveals <c>launch-recover-button</c> only once this resolves). A
    /// dead saved nest no longer hides the button: the read falls back to the
    /// device's own store (<c>DeploymentSeedCustody.LoadRecoverableBoxesAsync</c>
    /// never throws), so a surviving device still offers every box it custodies.
    /// </summary>
    /// <summary>
    /// <c>launch-identity-label</c>'s text for <c>launch_progress</c> and
    /// <c>launch_retry</c>: the bound (else active) account's cached
    /// <c>handle@domain · tier</c>, the shared <c>FfiAccountRegistry.LaunchIdentityLine</c>
    /// (<c>onboarding.md</c> § App-launch routing → <i>The cached identity on the launch
    /// surfaces</i>) — the call tui, linux, android and FaunaKit make. Null while the cache
    /// holds no handle, and on a registry read that failed (logged): the label is then
    /// absent, and the surface itself still paints.
    /// </summary>
    private static string? ReadLaunchIdentityLine()
    {
        try
        {
            using var registry = CredentialStore.Registry();
            return registry.LaunchIdentityLine();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[launch] the cached identity could not be read: {ex.Message}");
            return null;
        }
    }

    private static async Task RevealLaunchRecoverButtonAsync(Views.LaunchRetryPage page, string nestUrl, string secretHex)
    {
        byte[] ownerSecret;
        try
        {
            ownerSecret = Convert.FromHexString(secretHex);
        }
        catch (FormatException)
        {
            return;
        }
        var boxes = await Core.Helpers.DeploymentSeedCustody.LoadRecoverableBoxesAsync(nestUrl, ownerSecret);
        page.ShowRecoverButton(boxes);
    }

    /// <summary>
    /// <c>launch-retire-button</c> (<c>nest-retirement.md</c> § Layout &amp; flow):
    /// open <c>nest_retire</c> over the launch retry surface, in the root frame
    /// that holds it. No session is up, so the page reads no live domains and
    /// leaving it can never drop one: it re-dispatches the launch machine's
    /// current snapshot (<paramref name="putRetryBack"/>), which paints the retry
    /// surface again — its recovery entry re-read and its retire entry with it.
    /// </summary>
    private static void OpenNestRetireFromLaunch(Frame rootFrame, string nestUrl, Func<Task> putRetryBack)
    {
        rootFrame.Navigate(typeof(Views.Onboarding.NestRetirePage), new Views.Onboarding.NestRetireContext(
            RetireEntry.Launch,
            nestUrl,
            () => Task.FromResult<IReadOnlyList<string>>(Array.Empty<string>()),
            sessionDropped => { _ = PutRetrySurfaceBackAsync(putRetryBack); }));
    }

    private static async Task PutRetrySurfaceBackAsync(Func<Task> putRetryBack)
    {
        try { await putRetryBack(); }
        catch (Exception ex)
        {
            ShellLog.Error("App", $"[retire] putting the launch retry surface back failed: {ex.Message}");
            _currentErrorMessage = Core.Services.Strings.Error(ex);
        }
    }

    /// <summary>
    /// The WebSocket / MLS happy path: recreates the nest client against the
    /// now-known nest URL, opens the realtime WebSocket, initializes
    /// notifications + the MLS engine (key-package replenish, welcome
    /// processing), then navigates to <see cref="MainPage"/>. Called from the
    /// <c>Online</c> arm of <see cref="DispatchLaunchSnapshotAsync"/> (and from
    /// the post-onboarding handoff via that same dispatch). The
    /// <see cref="LaunchMachine"/> is passed to <see cref="DirectNestClient"/>
    /// (so it sources the bearer from the machine, falling back to its own
    /// FFI <c>mint_bearer</c> self-acquire only if the machine is wedged) and
    /// carried into the <see cref="ServiceClients"/> built for <c>MainPage</c>;
    /// this method also spawns the C#-side TTL refresh loop
    /// (<see cref="RunTtlRefreshLoopAsync"/>) so a long-lived session keeps a
    /// fresh bearer (mirrors <c>fauna-launch-machine</c>'s <c>ttl_refresh_loop</c>,
    /// which isn't exported via UniFFI — a Phase-2 concern).
    /// </summary>
    private async Task StartMainAppAsync(
        Frame rootFrame,
        ISessionAccount account,
        string secretHex,
        string baseUrl,
        LaunchMachine machine)
    {
        // (No `_mainAppStarted = true` here any more: the flag is now derived from
        // the root frame's actual page — see _mainAppMounted. Setting it at the TOP
        // of this method also claimed "the main app is up" for the ~250 lines of
        // awaited setup below it, i.e. before the Navigate at the end had run.)

        // Record the signed-in account so every ActiveActorHex reader scopes to
        // the right identity. This hook is the single site every login AND
        // account switch funnels through, so one assignment here covers both.
        // account-scoping.md § Serialized switching.
        ActiveActorHex = _cryptoService is { HasKey: true } c ? c.ActorIdHex : null;

        // Auto-start at sign-in (default ON): (re-)register the per-user Run-key
        // entry at this universal post-auth hook, so the first successful login
        // wires the app into every subsequent Windows sign-in — which is what
        // keeps the app-coupled sync agent provisioned and the shell-ext badges
        // live with no manual step. The pure AutoStartGate decides (E2E-disabled;
        // an explicit settings opt-out is never overridden); re-registering with
        // the current exe path self-heals a moved/upgraded install. Best-effort.
        // apps/windows.md § App Lifecycle → Auto-start at sign-in.
        AutoStartService.EnsureRegisteredAtLogin(new AppSettingsStore().AutoStartChoice);

        // Recreate the nest client against the now-known nest URL, wired to the
        // launch machine — DirectNestClient.EnsureAuthAsync sources the bearer
        // from machine.CurrentBearer() / machine.RefreshToken(), falling back to
        // its own FFI mint_bearer self-acquire only if the machine is wedged.
        // Dial the resolved (test-override-aware) URL, same as the initial
        // connect above — `baseUrl` itself stays the literal for every other
        // reader (onboarding.md § the dial seam).
        var dialUrl = uniffi.fauna_launch_machine.FaunaLaunchMachineMethods.ResolvedDialUrl(baseUrl);

        // Release-before-build for the clients themselves — same shape as the
        // _liveConvSession release-before-build a few lines below, and
        // the same reasoning: this is the single site every login AND same-process
        // re-entry funnels through (the comment above ActiveActorHex), so guarding
        // it here closes every re-entry that reaches here with a live predecessor
        // still hosted — factory-reset → re-claim and an IdentityChanged/Trust
        // same-actor re-entry (neither calls DisposeNestClients()), and sign-out →
        // wizard → re-login (never disposes its clients either — see
        // DisposeNestClients's own doc comment).
        // SwitchAccountHandler's explicit DisposeNestClients() call already nulls
        // both fields before reaching here, so this is a no-op on that path, not a
        // double dispose. Switch-shaped (the default): every re-entry that reaches
        // this line arrives AFTER its own erase already ran (a real sign-out's
        // ReleaseAccountScopedStoresBeforeErase, or a factory-reset's own wipe) —
        // this is stale-client cleanup before a fresh login, never the erase itself,
        // so retiring the enrollment again here would find nothing to retire.
        if (_rpcClient is { } oldRpcClient) Core.Services.AccountRuntimeStops.Process.Track(StopAccountRuntimeThenDisposeAsync(oldRpcClient));
        if (_nestClient is { } oldNestClient) _ = oldNestClient.DisposeAsync();
        _nestClient = new DirectNestClient(dialUrl, _cryptoService!, machine);
        _rpcClient = NewRpcClient(dialUrl, _cryptoService!);

        // box-recovery.md § The plane-era recovery floor, (c) The writes: run the
        // deployment-seed custody leg for this just-connected box. This is the
        // client's UNIVERSAL post-auth hook — every login, fresh claim AND
        // returning-user relaunch funnel through StartMainAppAsync's single
        // "transition into Online" call site — and the leg is self-healing rather
        // than event-driven (a live plane entry answers with no round trip;
        // otherwise roster membership, the seed fetch and the merge), so riding
        // this hook is what lets a claim, a late-added co-admin and a late-added
        // device all converge the same way on their next connect.
        // Fire-and-forget; an unconfirmed custody (or a thrown call) raises the
        // recovery-custody warning banner — see RunDeploymentSeedCustodyLegAsync.
        //
        // This pass and every other one this hook fires rides _rpcClient, the
        // session's ONE authenticated socket (transport.md: one WebSocket per
        // actor). Until 2026-09-28 four of them each built a one-shot
        // FfiNestClient, so one session start dialled five sockets for one actor
        // and one e2e recovery journey spent the process's whole per-nest dial
        // burst (transport-connection.md § The dial budget).
        _ = RunDeploymentSeedCustodyLegAsync(_rpcClient);

        // encryption-at-rest.md § Capability tiering → Content-sealing epochs: refresh
        // this client's epoch schedule on every connect AND reconnect, riding the SAME
        // universal post-auth hook as the custody leg above: the shared UniFFI trigger
        // is started once per client and re-runs the idempotent
        // MailSettingsMachine::refresh_epoch_schedule after each reconnect (it no-ops
        // when mail isn't enabled, so starting it unconditionally here is correct).
        // Fire-and-forget, log-only — invisible plumbing with no user-facing surface
        // on any outcome. The e2e `session` door starts it too (it never reaches here).
        _ = Core.Helpers.MailEpochSchedule.StartAsync(_rpcClient);
        _ = Core.Helpers.SubscriberEncapsKeyPublish.PublishAsync(_rpcClient);

        // critical-alerts.md § Mechanism → How often the detector runs: the re-sweep
        // loop rides the SAME universal post-auth hook as the two calls above, and
        // never returns — StartMainAppAsync is process-lifetime, which is the scope
        // a loop that only stops at identity teardown needs. A loop also ends with
        // the client it rides, and the old _rpcClient was just disposed above, so a
        // same-identity re-entry starts the re-established loop on the new client
        // (StartForIdentity's split). Best-effort, fire-and-forget — never awaited
        // for sign-in to proceed.
        Core.Helpers.CriticalAlertsSweep.StartForIdentity(_rpcClient, baseUrl);

        // The S8 seal backfill (file-sync.md § Sealed names & paths → Implementation
        // status today): rides the same universal post-auth hook, through the just-
        // connected _rpcClient (no second BackupKey derivation — the S8 rule).
        // Best-effort, fire-and-forget — mirrors apple's runSealBackfill call site.
        _ = Core.Helpers.SealBackfillSweep.RunAsync(_rpcClient);

        // The post-succession aftermath (succession-aftermath.md § Re-key scope's
        // `BackupKey` corpus row: "started at first successor sign-in, surfaced with
        // progress, resumed until complete"). Rides the SAME universal post-auth hook
        // as the four passes above, on EVERY authenticated start rather than only one
        // that just ran a ceremony: the pass is resumable and no-ops for an identity
        // that never succeeded, and a re-seal cut short by a lost connection is
        // finished by the NEXT sign-in — which has no ceremony to notice.
        // Best-effort, fire-and-forget — mirrors apple's SuccessionAftermath.run.
        // The ceremony's raise context is parked durably in the account registry
        // by shared Rust and drained by the pass itself; no app carries it.
        _ = Core.Helpers.SuccessionAftermath.RunAsync(_rpcClient);

        // installers/README.md § Knowing a newer version is out: the ONE unasked,
        // notify-only look per sign-in, on the client that just signed in — it reads
        // that nest's newest_release, never the release host. A newer release paints
        // the same notice the asked check paints (Settings → General), a failed look
        // paints nothing. Fire-and-forget; it never holds sign-in up.
        _ = Core.Services.UpdateCheck.Shared.LookOnceAtSignInAsync(_rpcClient);

        // The onboarding wizard's sign-in follow-ups (the captured DNS credential,
        // the one-tap trust mint, the confirmed recovery kit), queued for THIS
        // actor at the wizard's LoggedIn terminal and run here on the session's
        // own client (PostSignInHandoff's header). Empty on every sign-in that did
        // not come through the wizard. Best-effort, fire-and-forget.
        _ = Core.Services.PostSignInHandoff.RunForAsync(ActiveActorHex, _rpcClient);

        // Push (common.md § Push Notifications → *Registration*): announce this
        // device on the session's own connection — the announce is opt-in per
        // caller — and re-arm this install's ws-device row when it opted in, never
        // opting it in. Every login and switch-in funnels through here, so the
        // incoming identity re-arms with no Settings visit. Best-effort.
        _ = Core.Services.PushSession.OnSessionStartAsync(_rpcClient, ActiveActorHex, account.ResolveDeviceId());

        // NO in-app backup upload driver: the SOURCE NEST is the segment-backup writer
        // (message-segment-store.md § Cross-location backup protocol — nest-side writer
        // BUILT 2026-07-24; the client-driven pass "retires wholesale at the slice-5
        // flip", which this app's arm completed 2026-08-16, last of the seven). It backs
        // every enrolled owner up with no client awake, so an app that still drove
        // run_forever here could only double-write or diverge. The Backups page reads
        // per-destination status from the nest's own fauna.backup.status projection
        // instead — live with the app closed. Do NOT re-add a driver to make Settings →
        // Task delegation show a client runner for `backup-upload`: that row is meant to
        // show the nest (backup-restore.md § Background Tasks → Flip status (slice 5)).

        // On-demand hydration handoff: start the SESSION-SCOPED provisioning
        // convergence loop (agent running + ProvisionCapability when the agent
        // reports none + RefreshBearer each tick) instead of a once-at-login push —
        // a restarted agent recovers within one tick with no app restart. The
        // identity seed never leaves the app — CapabilityProvisioner derives the
        // BackupKey via the backup_key_derive FFI and sends only that. Everything
        // is best-effort (the helper may not be running; on-demand sync is opt-in)
        // and off the UI thread, so it never disrupts login. Gated so deterministic
        // e2e (FAUNA_E2E_BRIDGE without FAUNA_E2E_REAL_SYNC_AGENT) never spawns or
        // provisions a real agent — an onboarding e2e that reaches this path must
        // not write into the box's installed fauna-sync-agent.exe. on-demand-files.md
        // § On-Demand Files; windows.md § On-demand hydration host.
        if (HydrationSessionEnabled)
        {
            var machineForHydration = machine;
            StartHydrationSession(
                // The machine's cached bearer, read fresh on every convergence
                // tick — the cheap synchronous read the shared loop specifies
                // (an empty/absent token skips the tick). The old async fallback
                // to `GetBearerTokenAsync` is gone with the C# twin: no acquire
                // belongs on a loop tick, and RunTtlRefreshLoopAsync's Poke below
                // already wakes the loop the moment a bearer lands, so a first
                // tick that finds none costs nothing.
                bearerSource: () =>
                {
                    // Token and deadline (the machine's own clock, anchored at
                    // receipt) travel together; a bearer without a readable
                    // deadline skips the tick rather than hand the agent a guess.
                    var token = machineForHydration.CurrentBearer();
                    if (token is null) return null;
                    if (machineForHydration.Snapshot().@token is not TokenStatus.Valid tv) return null;
                    return (token, tv.@expiresAtSecs);
                },
                rpc: _rpcClient, deviceId: account.ResolveDeviceId() ?? "");
        }

        NotificationService.Initialize();
        EnsureAgentAttachment();
        // OS notifications re-homed onto WS-RPC push (was the dead WebSocketService):
        //  • DM toasts    → MessageToastObserver over the ConversationsManager (below).
        //  • Knock toasts  → the knock pump raises KnockReceived on each fauna.knock.
        // (ContactsPage separately subscribes KnockReceived to refresh its roster.)
        // conversations.md § Where logic lives — OS notifications are client glue.
        _rpcClient.KnockReceived += NotificationService.ShowKnockNotification;

        // Host the W3 (account-data-plane.md § Workstreams) account-store runtime (see the method). Independent of
        // the conversations session below — no ordering contract between the
        // two — and unconditional on E2eEnv.Bridge (unlike that block): a
        // hybrid launch that reaches this path under a bridge still owes the
        // account plane a host. account-data-plane.md § The account store →
        // *The client-side lifecycle*.
        await _rpcClient.StartAccountRuntimeAsync(
            Core.Services.AccountStateDir.Base, account.ResolveDeviceId());

        // Conversations: build the shared-Rust ConversationsSession OVER the
        // process-wide ConversationsManagerHost.Instance (never a fresh internal
        // manager) and replenish the key-package pool off it. ALL MLS ops run in
        // shared Rust — no client-side MLS state (conversations.md § Architectural
        // rules #2). ConversationsManagerHost holds the ONE manager for the
        // process; building over it (rather than swapping it in via
        // RegisterRealManager afterwards) is what keeps anything ever injected
        // into it from being silently orphaned (testing.md § Cross-app e2e
        // conventions, convention 10). E2E uses ConversationsManagerHost.Instance
        // (mock backends) instead, so skip the real session under FAUNA_E2E_BRIDGE.
        //
        // Release-before-build (account-runtime.md § Multi-instance concurrency →
        // *The role is HANDED OVER in-process*; apple's ConversationsVM.rebuild() is
        // the reference shape): a re-login within this
        // process — sign-out → wizard, factory-reset → re-claim, or an
        // IdentityChanged/Trust same-actor re-entry — can reach this method again
        // without an intervening DisposeNestClients(), which would otherwise
        // silently overwrite a live predecessor rather than retire it.
        RetireConvSession(null, _liveConvSession);
        _liveConvSession = null;
        ConversationsSession? convSession = null;
        if (string.IsNullOrEmpty(E2eEnv.Bridge))
        {
            try
            {
                // Pass "" (never null) when identity hasn't resolved — an explicit
                // empty string, not the omitted-parameter default, so production
                // NEVER falls through to BuildConversationsSessionAsync's resolve-
                // based e2e door (that door exists only for the e2e caller, which
                // has no LaunchMachine at all).
                convSession = await _rpcClient.BuildConversationsSessionAsync(
                    FaunaApp.Conversations.ConversationsManagerHost.Instance,
                    identityDomain: machine.Snapshot().@identity?.@domain ?? "",
                    // The `index`-lease seat (participants.md § Coordination primitive
                    // → *The `index` kind under the lease*). Read from the store rather
                    // than `_deviceId` so the value is the persisted one on EVERY login
                    // path into here, including the ones that never set the field.
                    deviceIdHex: account.ResolveDeviceId());
                if (convSession is not null)
                {
                    // Give the session an owner that outlives this local, so the
                    // actor-change teardown can close it (DisposeNestClients).
                    _liveConvSession = convSession;
                    // conversations.md § State & data shape → Self-address: live, never
                    // baked — attach the just-built session to this launch's heal observer
                    // so a LATER identity resolve/change (rename, a late silent-challenge
                    // result) reaches SetSelfAddress with no re-navigate. Also applies
                    // whatever identity is already resolved right now, covering the race
                    // where it landed between machine construction and this point.
                    _selfAddressHealObserver?.AttachSession(convSession);

                    var mgr = convSession.Manager();
                    // (Login-time keypackage replenish is session-owned: the shared
                    // StartReceiveLoop runs it AFTER the MLS replica restore — a
                    // restore swaps the engine's provider storage, so a package
                    // minted before it would lose its private init key; devices.md
                    // § Cross-device MLS group-state sync.)
                    //
                    // Start the shared-Rust push-driven receive loop: it subscribes
                    // the inbound welcome.received + channel.message pushes and drives
                    // ingest_welcome + the channel poll into the wired manager (the
                    // native twin of linux conv_backend.rs; conversations.md §
                    // Receiving into the conversations view, § MLS Welcome at-rest).
                    // It spawns a detached tokio task and returns, so this awaits only
                    // the start. No ConfigureAwait(false) (WinUI bound-state rule).
                    try { await convSession.StartReceiveLoop(); } catch { /* best-effort */ }

                    // Phase two's archive links (archive-import.md § Linking and
                    // merging): built and registered as the session's sink by the FFI
                    // factory above; drive them now and on the host's tick. The e2e
                    // login path starts them through the same helper.
                    await StartArchiveLinksAsync(_rpcClient);
                    // The Archive rail's first read (archive-import.md § What each
                    // category becomes): the factory registered the source; the
                    // conversations page re-reads it on every visit. The e2e login
                    // path reads it through the same helper.
                    _ = RefreshArchiveThreadsAsync(convSession);

                    // App-lifetime DM OS-toast observer — see AttachMessageToastObserver.
                    // The e2e login path attaches the SAME observer through the same
                    // helper (BuildE2eConvSessionAsync), so the witness sees this wiring.
                    AttachMessageToastObserver(mgr);

                    // Draft-persistence v2, conversations leg (file-sync.md § Drafts
                    // Sync; conversations.md § Persistence): build the nest-backed
                    // __drafts autosync (the shared DraftsSync wrapper — launch gate +
                    // last-saved baseline owned in Rust, same as web/linux/android) +
                    // service over this session's manager, restore any stored conversation
                    // drafts INTO the manager before the page loads, and expose the service
                    // so the conversations page saves (debounced) after a compose change.
                    // Shared with the e2e harness via the helper.
                    await WireConversationDraftsAsync(_rpcClient, mgr);
                }
            }
            catch { /* conversations is optional — the page still renders */ }
        }
#if DEBUG || FAUNA_E2E_AGENT
        else
        {
            // Seeded e2e launches (App.OnLaunched's seed_credentials shape — the
            // coexistence tests' login) reach this SAME path but previously built
            // NO real session at all, so the shared served_elsewhere arming
            // (nest_client.rs's set_engine_served_elsewhere, called inside
            // conversations_session_over_manager) never fired for them — the gap captured (windows was the only one of the
            // seeded/set_state login paths that skipped it entirely). Reuse
            // BuildE2eConvSessionAsync rather than re-deriving its gate (build
            // always; receive-loop/real-rails/drafts only under
            // FAUNA_E2E_REAL_CONVERSATIONS) — mirrors linux
            // conv_backend::start_conversations_session, which runs the real
            // session under e2e for EVERY login, seeded or not.
            convSession = await BuildE2eConvSessionAsync(_rpcClient, account.ResolveDeviceId());
            if (convSession is not null)
            {
                // Same two wires the production branch above sets — teardown
                // ownership and the self-address heal observer — so a seeded e2e
                // login's session is exactly as live as production's.
                _liveConvSession = convSession;
                _selfAddressHealObserver?.AttachSession(convSession);
            }
        }
#endif

        // Draft-persistence v2, feed leg (reserved-folders.md § Drafts Sync;
        // feed.md § Persistence): build the nest-backed __drafts autosync (rail
        // "posts") so FeedPage.Page_Loaded can pair it with each freshly-built
        // FfiFeedManager. Unconditional (unlike the conversations wiring above,
        // feed does not depend on a real conversations session existing).
        await WireFeedDraftsAsync(_rpcClient);

        // Draft-persistence v2, events leg (reserved-folders.md § Drafts Sync;
        // events.md § Persistence): build the nest-backed __drafts autosync (rail
        // "events") so EventsPage's first load can pair it with its own compose
        // surface. Unconditional, same as the feed wiring above.
        await WireEventDraftsAsync(_rpcClient);

        // Reconnect re-hydrate (transport.md § Push events): drive a pump over the
        // shared FfiNestClient.SubscribeReconnects watch that raises
        // INestRpcClient.Reconnected on every WS reconnect, so the live surface VMs
        // (feed / notifications / contacts) re-fetch — the feed has no poll
        // backstop, so a post that arrived while disconnected would otherwise stay
        // invisible until a manual refresh. Started here on the UI thread (the pump
        // captures this SynchronizationContext to marshal the event to bound VMs),
        // unconditionally — the benign-flip re-hydrate must work under the E2E
        // bridge too. Idempotent + app-lifetime; reuses the page VMs' WS-RPC
        // connection (the same _rpcClient). Native twin of linux's reconnect pump
        // (apps/fauna-linux/src/client.rs → WsEvent::Reconnected).
        _rpcClient.StartReconnectPump();
        // The session's identity follow (long-term-store.md § Cross-app server-data
        // cache): every identity this session's own bearer mint confirms lands in the
        // account's cached handle/domain/tier, which the Settings identity cell and the
        // launch surfaces paint. Idempotent; ends with the client.
        _rpcClient.StartSelfIdentityFollow();
        // Knock push pump (app-lifetime): drives the OS knock toast + the contacts
        // roster refresh off the shared FfiNestClient.SubscribeKnocks stream. Twin of
        // linux app.rs WsEvent::Push(PushEvent::Knock). Started on the UI thread so
        // KnockReceived is marshaled there for the bound roster refresh.
        _rpcClient.StartKnockPump();
        // Generic push pump (app-lifetime, transport.md § Push events): drives the
        // one central fauna.notification / fauna.calendar.changed /
        // fauna.protocol.resync_required dispatch off the shared
        // FfiNestClient.SubscribePushes stream (the twin of linux app.rs
        // WsEvent::Push for every other modelled kind). Started on the UI thread so
        // Reconnected / CalendarPushChanged are marshaled there for the bound VMs.
        _rpcClient.StartPushPump();
        // Author-side subscriptions reconciliation (monetization.md § The unifying
        // model, grant path 2): resume crash-staged removals, then drain queued
        // auto-approve subscribes — the loop that makes an encrypted-mode follow
        // auto-grant with no manual approve (the nest can't mint; the author's
        // client picks the queue up on connect + a poll backstop). Headless,
        // best-effort, client-instance lifetime; also kicked from the TestAgent
        // set_state login, which never reaches this method.
        _rpcClient.StartSubscriptionsAuthorPump();

        // Admin host-address reporting (domains-and-tls-bootstrap.md § Host-address
        // acquisition): the admin client reports the nest's PUBLIC IP
        // (fauna.dns.set_host_address) once on connect so ACME HTTP-01 gates on the
        // STRONG resolve-check. am_i_admin-gated (a non-admin call is refused
        // nest-side) + idempotent (last-writer-wins converges an IP change); the
        // classify + never-publish-a-private-address safety live entirely in the
        // shared FFI fn (priority #2). Fire-and-forget, best-effort — the native
        // twin of linux's AdminStatusLoaded → report_host_address() / web
        // reportHostAddress. The E2E set_state login never enters StartMainAppAsync,
        // so its cmd-session path runs the same reporter itself.
        _ = new HostAddressReporter(_rpcClient).RunAsync();

        // Muted-keywords cache seed (moderation.md § Muted keywords): populate
        // MutedKeywordsCache once at login so the conversation bubble collapse
        // works on a fresh launch without visiting Settings → Muted words first
        // (mirrors linux's auth-time FaunaClient::load_muted_keywords seed).
        // Fire-and-forget, best-effort. Reached only in production (the E2E
        // set_state login never enters StartMainAppAsync); e2e populates the
        // cache by driving the real Settings UI instead.
        _ = new MutedKeywordsPreloader(_rpcClient).RunAsync();

        // fauna-launch-machine doesn't export ttl_refresh_loop (Phase-2 concern;
        // per-app adoptions wrap it in platform-native scheduling), so spawn
        // the C# equivalent here — fire-and-forget, app lifetime: the loop
        // self-exits when the machine leaves Online (sign-out / terminal
        // Offline), and process exit ends it; no cancellation token needed.
        // This is the single "transition into Online" site, so one spawn here is
        // exactly the right cardinality. Stashed for TriggerSilentSignInForTestAsync
        // (see _liveTtlRefreshContext's doc) at the same site, for the same reason.
        _liveTtlRefreshContext = (machine, rootFrame, account, secretHex, baseUrl);
        _ = RunTtlRefreshLoopAsync(
            machine, rootFrame, account, secretHex, baseUrl,
            onBearerRefreshed: () => _syncAgent.Current?.Poke());

        // Native-only background TLS-cert auto-renew cadence (tls-certificates.md
        // § C.3): any synced admin device periodically re-issues the at-risk ∧
        // auto-renew-on domains (DnsSnapshot::domains_needing_auto_renew) with no
        // admin tap — the native twin of linux's run_auto_renew_cadence_tick loop.
        // Fire-and-forget, but ACTOR-scoped, not app-lifetime: it holds an
        // ActorScope lease and the next actor change retires it (see its header —
        // spawning one per Online transition with no seam is exactly how windows
        // came to accumulate a live cadence per login), and it is one per client
        // and actor generation, so a second spawn here or at the e2e login's seam
        // joins the live one. It runs under the E2E bridge too: it rides the
        // session's own connection (no second WS), and its 6 h first wait is what
        // the e2e `auto_renew_wake` poke races (tls-certificates.md § C.3, the
        // tick witness).
        _ = RunAutoRenewCadenceAsync(_rpcClient);

        // The blessed-nests trust auto-renew loop (nests.md § Expiry / renewal →
        // Duration and blessing): app start, then every AUTO_RENEW_CHECK_SECS,
        // whichever page the user sits on. Unlike the DNS cadence above it rides
        // the session's shared WS-RPC connection (no second socket), so it also
        // runs under the E2E bridge — the blessed-renewal journey reads its effect.
        _ = RunNestsAutoRenewLoopAsync(_rpcClient);

        var navParam = new ServiceClients(
            _nestClient, _cryptoService, account, convSession,
            Launch: machine, Rpc: _rpcClient);
        rootFrame.Navigate(typeof(MainPage), navParam);
    }

    /// <summary>
    /// The trust facet's auto-renew loop (<c>nests.md</c> § Expiry / renewal →
    /// <i>Duration and blessing</i>): one hydrate at app start — its refresh runs
    /// the renew sweep, so a blessed nest's due grants renew as the app comes up —
    /// then <c>LinkedNestsAction.AutoRenew</c> every
    /// <c>AUTO_RENEW_CHECK_SECS</c> (the desktop rule; iOS dispatches at
    /// foreground only). Best-effort and silent, like the shared tick itself: a
    /// failure is logged and retried next cycle.
    ///
    /// <para>Actor-scoped like <see cref="RunAutoRenewCadenceAsync"/>: it holds an
    /// <see cref="Core.Services.ActorScope.BeginBackgroundLoop"/> lease, exits on
    /// the next actor change, and disposes the machine it owns.</para>
    /// </summary>
    private static async Task RunNestsAutoRenewLoopAsync(Core.Services.INestRpcClient rpc)
    {
        using var lease = Core.Services.ActorScope.BeginBackgroundLoop();
        var actorGone = lease.Token;

        uniffi.fauna_client_pair.LinkedNestsMachine machine;
        try
        {
            machine = await rpc.BuildLinkedNestsMachineWithMailRelayAndTrustAsync();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestsAutoRenew", $"could not start: {ex.Message}");
            return;
        }

        var interval = TimeSpan.FromSeconds(
            uniffi.fauna_client_pair.FaunaClientPairMethods.AutoRenewCheckSecs());
        try
        {
            try
            {
                await machine.Hydrate();
            }
            catch (Exception hx)
            {
                // The WS-RPC connection may not be up yet; the first tick retries.
                ShellLog.Warn("NestsAutoRenew", $"start hydrate failed: {hx.Message}");
            }
            while (!actorGone.IsCancellationRequested)
            {
                await Task.Delay(interval, actorGone);
                try
                {
                    await machine.Dispatch(new uniffi.fauna_client_pair.LinkedNestsAction.AutoRenew());
                }
                catch (Exception tx)
                {
                    ShellLog.Warn("NestsAutoRenew", $"tick failed: {tx.Message}");
                }
            }
        }
        catch (OperationCanceledException)
        {
            // The actor changed — the only way out of the loop; not an error.
        }
        finally
        {
            machine.Dispose();
        }
    }

    /// <summary>
    /// Whether the session-scoped hydration provisioning loop may drive a sync
    /// agent: always in production, and under an e2e bridge whenever the harness
    /// has said WHICH agent to drive.
    ///
    /// <para>
    /// The rule the three arms encode is <i>never the box's installed agent</i> —
    /// not <i>never an agent</i>. That distinction is the 2026-09-21 ruling
    /// (<c>e2e-conventions.md</c> convention 10): windows had been the one desktop
    /// whose e2e login provisioned nothing, while linux and tui both spawn the real
    /// agent binary as an isolated per-launch child, so windows alone never
    /// exercised the production sync bring-up — and, because the machine's named
    /// <c>sync_devices</c> row is registered by that bring-up's renewal-grant mint,
    /// no windows e2e could assert a named-row invariant at all.
    /// </para>
    ///
    /// <list type="bullet">
    /// <item>No bridge → production; the agent is the per-user installed one.</item>
    /// <item><c>FAUNA_E2E_SYNC_PIPE</c> + <c>FAUNA_E2E_SYNC_AGENT_BIN</c> → the run
    /// pinned its own pipe and its own binary, so the agent we spawn is this run's
    /// and no other. Requiring BOTH is what makes the isolation structural: shared
    /// Rust spawns nothing when a pin misses, where an unpinned spawn would walk its
    /// candidates down to <c>%ProgramFiles%</c>
    /// (<c>agent_spawner::WindowsDetachedSpawner</c>).</item>
    /// <item><c>FAUNA_E2E_REAL_SYNC_AGENT</c> alone → the installer full journey,
    /// which tests the INSTALLED product and therefore MUST rendezvous on the
    /// machine-global per-SID pipe. Its foreign-agent guard
    /// (<c>helpers/windows_sync_agent.py</c>) is what keeps that honest.</item>
    /// </list>
    ///
    /// <para>
    /// Teardown is already armed and needs nothing here: the spawner is deliberately
    /// not <c>CREATE_BREAKAWAY_FROM_JOB</c>, so the agent stays inside the harness's
    /// job object, and the windows driver reaps the whole
    /// bridge → FaunaApp → fauna-sync-agent tree
    /// (<c>drivers/windows.py</c>'s <c>reap_descendants_of</c>, convention 9).
    /// </para>
    /// </summary>
    private static bool HydrationSessionEnabled =>
        string.IsNullOrEmpty(E2eEnv.Bridge)
        || (!string.IsNullOrEmpty(E2eEnv.SyncPipe) && !string.IsNullOrEmpty(E2eEnv.SyncAgentBin))
        || !string.IsNullOrEmpty(E2eEnv.RealSyncAgent);

    /// <summary>The agent attachment lease, held for this process's life once the first
    /// session starts (<see cref="EnsureAgentAttachment"/>).</summary>
    private static uniffi.fauna_ffi.FfiAgentAttachment? _agentAttachment;
    private static readonly object AgentAttachmentGate = new();

    /// <summary>
    /// Hold the sync agent's attachment lease for the rest of this process's life — "this
    /// app is open on this machine" — so the agent's <c>ws-device</c> push arm leaves the
    /// banners to this app's own toasts while it runs (<c>common.md</c> § Push
    /// Notifications → <i>Transports</i>; <c>windows.md</c> § Notifications). The attach
    /// names this app's toast identity (<see cref="NotificationService.Identity"/>), the
    /// AUMID the agent posts its toast under while the app is closed. Shared Rust owns the
    /// connection and the re-attach across agent restarts
    /// (<c>fauna_client_sync::attachment</c>, the one tui links); the lease ends with the
    /// process, a crash included. Gated like the provision path, so an e2e launch with no
    /// harness-pinned agent never attaches to the box's real one. Once per process: an
    /// account switch keeps the app open.
    /// </summary>
    private static void EnsureAgentAttachment()
    {
        if (!HydrationSessionEnabled) return;
        lock (AgentAttachmentGate)
        {
            if (_agentAttachment is not null) return;
            NotificationService.Initialize();
            try
            {
                _agentAttachment = uniffi.fauna_ffi.FaunaFfiMethods.AttachToSyncAgent(
                    "windows", NotificationService.Identity);
            }
            catch (Exception ex)
            {
                ShellLog.Warn("App", $"[agent-attach] not attached: {ex.Message}");
            }
        }
    }

    /// <summary>
    /// Start (or restart) the session-scoped <see cref="FaunaApp.Core.Services.HydrationSessionService"/>:
    /// each tick it ensures the per-user sync agent is running (probe the per-session
    /// pipe, spawn fauna-sync-agent.exe detached if not — file-sync.md § app-coupled auth),
    /// pushes <c>RefreshBearer</c>, and on the agent's "no capability provisioned"
    /// reply mints + pushes the full Phase-1 capability (owner <c>BackupKey</c> +
    /// public <c>actor_id</c> + bearer + resolved shared-set engine keys, 5d(c)) so
    /// the cfapi placeholder host (re)starts. The identity seed never leaves the app —
    /// <see cref="CapabilityProvisioner"/> derives the <c>BackupKey</c> via the
    /// <c>backup_key_derive</c> FFI export and sends only that
    /// (key-material-hierarchy.md rule #7). Stopped in <see cref="DisposeNestClients"/>
    /// (logout / reset / nest re-point). Per on-demand-files.md § On-Demand Files;
    /// windows.md § On-demand hydration host.
    /// </summary>
    /// <summary>
    /// Drop this app's handle on the agent WITHOUT unprovisioning it — the agent
    /// keeps serving. Bumps the generation so an in-flight
    /// <see cref="StartHydrationSession"/> installs nothing.
    /// </summary>
    private void StopSyncAgentSession()
    {
        // The control plane belongs to the login, not the agent handle: drop it here, where
        // the login ends, and let StartHydrationSession mint a fresh one for the next.
        _locationBindings = null;
        ClearOfflineShareSession();
        _syncAgent.Stop();
    }

    /// <summary>Dispose and reset the offline co-present share ceremony session
    /// state (<see cref="CurrentOfflineShareSeat"/>) — called everywhere
    /// <c>_locationBindings</c> is cleared, for the same reason: a seat bound
    /// under a previous account's secret must not survive into the next
    /// login. A no-op in the store-safe flavor, which has no ceremony.</summary>
    private void ClearOfflineShareSession()
    {
#if P2P_SHARE
        _offlineShareSeat?.Dispose();
        _offlineShareSeat = null;
        _offlineSharePanel = uniffi.fauna_client_capabilities.OfflineSharePanel.Closed;
        _offlineShareStatus = uniffi.fauna_client_capabilities.CeremonyStatus.Idle;
        _offlineShareExpectingFrom = null;
        // The account runtime's teardown forgets the plane's cell; re-read so the
        // peer-transfer surface disappears with the session it belonged to.
        FaunaApp.Core.Services.SharePlaneModel.Shared.Refresh();
#endif
    }

    /// <summary>
    /// Stop the loop and tell the agent to drop its capability — the sign-out /
    /// account-switch / factory-reset teardown (on-demand-files-apple.md § Multi-account ×
    /// File Provider, consequence 1). The shared provisioner stops the loop
    /// before it unprovisions, so no tick can re-provision from still-loaded
    /// crypto and undo it. Best-effort: an unreachable or never-started agent is
    /// a normal no-op.
    /// </summary>
    private Task UnprovisionSyncAgentAsync()
    {
        _locationBindings = null;
        ClearOfflineShareSession();
        return _syncAgent.UnprovisionAsync();
    }

    private void StartHydrationSession(
        Func<(string Token, ulong ExpiresAt)?> bearerSource,
        NestRpcClient rpc, string deviceId)
    {
        // Supersede the outgoing session SYNCHRONOUSLY, here on the caller's thread, before
        // the install below is queued. InstallAsync supersedes too (harmlessly — the
        // generation bump is idempotent and it will find nothing left to stop), but doing
        // it only there would move the bump inside the Task.Run: two logins in quick
        // succession would then take their generations in thread-pool order rather than
        // call order, and the LATER login could lose.
        StopSyncAgentSession();
        var crypto = _cryptoService!;
        // This login's actor, read once here while its key is loaded: it stamps the session
        // (SyncAgentSession.ActorIdHex).
        var actorIdHex = crypto.ActorIdHex;
        // Retired owner keys after an identity succession (sync-agent.md § Credential
        // model) — resolved ONCE here, post-auth, and handed to the provisioner build
        // below (mirrors linux's client.rs::predecessor_backup_keys, the one cached
        // resolution label_custody() and sync_agent.rs::install() both share, and
        // tui's session.rs post-auth hook). A local credential-store read, not a nest
        // round-trip, so resolving it fresh on every hydration (login, re-point,
        // reconnect) rather than caching across the whole process lifetime costs
        // nothing and picks up predecessor material that arrived mid-session sooner.
        // Fails CLOSED on any error — the same empty-list posture the stopgap this
        // replaces already had (a successor's pre-succession corpus stays unopenable
        // this hydration, never a login-blocking failure).
        // The attested actor ids beside the keys above (`account-data-taxonomy.md`
        // § The generation machinery → *The source of `prior`*, ruled 2026-09-13) —
        // resolved off the SAME registry instance, once, and handed to the
        // provisioner build below alongside `predecessorBackupKeys`. Empty for an
        // identity that never succeeded, which is fail-safe.
        byte[][] predecessorBackupKeys;
        byte[][] predecessorActorIds;
        try
        {
            using var predecessorRegistry = CredentialStore.Registry();
            predecessorBackupKeys = predecessorRegistry.PredecessorBackupKeys(crypto.ActorIdHex);
            predecessorActorIds = predecessorRegistry.AttestedPredecessorActorIds(crypto.ActorIdHex);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[sync-agent] predecessor-backup-keys resolve failed (empty fallback): {ex.Message}");
            predecessorBackupKeys = Array.Empty<byte[]>();
            predecessorActorIds = Array.Empty<byte[]>();
        }
        // The control plane comes up NOW, synchronously, ahead of the whole asynchronous
        // start-up chain below. This is the fix for a real user-facing bug: binding a folder
        // within ~10 s of signing in used to find a null session, take the refusal path and
        // be dropped — one-shot, never retried. The controller records the row immediately,
        // renders it, and pushes it the moment the session attaches its channel.
        var bindings = new FaunaApp.Core.Services.LocationBindingsController();
        _locationBindings = bindings;
        // Fire-and-forget: the whole chain (nest connect, grant mint, first
        // convergence tick) is best-effort and must never sit in front of
        // login. A failure leaves the host's Current null, which every consumer
        // already treats as "no local agent".
        //
        // The install ORDERING — supersede, build, publish, then start — belongs to
        // SyncAgentSessionHost, which is where it is unit-tested. Two log-visible facts
        // it guarantees, both load-bearing when reading a session's start-up in the app
        // log: the session is published (and so reachable by sign-out) BEFORE it
        // provisions, and a build superseded mid-flight is discarded having provisioned
        // nothing at all.
        _ = Task.Run(() =>
        {
            // Bracket the whole start-up chain in the log: a chain that merely runs LONG
            // is otherwise indistinguishable downstream from one that failed, and both
            // read as a broken engine rather than an unfinished start-up. The
            // ENTER/INSTALLED pair is what tells those apart in one run (an ENTER with no
            // INSTALLED = still in flight).
            ShellLog.Info("App", "[sync-agent] session start ENTER");
            return _syncAgent.InstallAsync(
                build: async () =>
                {
                    var session = await FaunaApp.Core.Services.SyncAgentSession.CreateAsync(
                        // The login's own live, connected client — never a private one
                        // (SyncAgentSession.CreateAsync's remarks say why).
                        rpc,
                        actorIdHex,
                        deviceId,
                        predecessorBackupKeys,
                        predecessorActorIds,
                        bearerSource,
                        onSyncComplete: fileName => NotificationService.ShowSyncCompleteNotification(fileName),
                        // The agent-up edge re-drives the folder-binding reconcile. Without it,
                        // bindings made before the agent came up (the first post-upgrade launch)
                        // sit unpushed until the next user gesture — the reconcile only
                        // fires once, at attach. It is driven on the SESSION's
                        // controller, never on the Folders page: the page's notifier is by
                        // contract a no-op while it is closed, which is most of the time, so a
                        // page-routed edge pushed nothing in exactly the case this exists for.
                        onAgentReachable: () => _ = bindings.ReconcileAsync())
                        .ConfigureAwait(false);
                    ShellLog.Info("App", $"[sync-agent] session BUILT (null={session is null})");
                    return session;
                },
                onInstalled: session =>
                {
                    // The control plane finally has an agent: adopt the channel and reconcile,
                    // which pushes every binding recorded during the start-up window above.
                    bindings.AttachChannel(session.LocationControl);
                    ShellLog.Info("App", "[sync-agent] session INSTALLED");
#if P2P_SHARE
                    // The peer-transfer plane (p2p.md § Cross-user shared-set transfer),
                    // fired detached from where the provisioner starts — exactly as macOS
                    // does from startSyncAgentProvisioner. No ordering against
                    // StartAccountRuntimeAsync / BuildConversationsSessionAsync: shared Rust
                    // waits (bounded) for both. Best-effort: SharePlaneModel logs a failure
                    // and the surface stays absent.
                    _ = FaunaApp.Core.Services.SharePlaneModel.Shared.StartAsync(
                        rpc, session.Provisioner,
                        System.IO.Path.Combine(BackupPaths.DataDir, "share-spool"));
#endif
                    // No content-key push: the agent resolves every set's keys from the
                    // account's custody itself (on-demand-files-shared-sets.md § Shared sets on a
                    // capability host → One mechanism).
                });
        });
    }

    /// <summary>
    /// The post-claim serving enablement — the ONE shared-Rust step every app's
    /// <c>LoggedIn</c> handoff makes with the four intents it read off the
    /// onboarding machine (onboarding.md § 3b <i>Mechanism</i>;
    /// <c>fauna_client_mail_settings::serving_enablement</c>), fired over the
    /// session's own connection once the authenticated session exists (reached only on
    /// fresh onboarding; returning users launch via the launch-machine path).
    /// <para>
    /// No enablement decision lives here. The shared step resolves
    /// <c>fauna.account.am_i_admin</c> itself and runs its fixed plan to the end: an
    /// admin claim honors the intents (the three DAV toggles, the one MSEK-minting
    /// path, the mail mint last — each in its own failure domain); a new non-admin
    /// user auto-mints their own mailbox iff the deployment policy allows
    /// (mail-credentials.md § Auto-enable for new users). Its completion, the
    /// decide-nothing run included, is published as the <c>serving_enablement</c>
    /// e2e state key (<c>ServingEnablementForSerialization</c>, compiled only with
    /// the e2e agent).
    /// </para>
    /// Best-effort: every step logs its own failure inside the shared step, so only
    /// a connect or secret-decode failure surfaces here — logged, never thrown into
    /// the handoff.
    /// </summary>
    private static async Task ApplyPostClaimServingEnablementAsync(
        Core.Services.INestRpcClient rpc, string nestUrl, bool enableMail, bool enableCaldav,
        bool enableCarddav, bool enableWebdav)
    {
        try
        {
            // Over the session's own connection (transport.md: one WebSocket per
            // actor); nodeUrl is the nest's literal URL, as before.
            await rpc.ApplyPostClaimServingEnablementAsync(
                nestUrl, enableMail, enableCaldav, enableCarddav, enableWebdav);
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine(
                $"[onboarding] post-claim serving enablement failed: {ex.Message}");
            ShellLog.Error("App",
                $"[onboarding] post-claim serving enablement failed: {ex.Message}");
        }
    }

    /// <summary>
    /// Native-only background TLS-cert auto-renew cadence (tls-certificates.md
    /// § C.3, Slice 4 C2) — the native twin of linux's
    /// <c>run_auto_renew_cadence_tick</c> loop in <c>start_ws_rpc</c>. Builds a
    /// dedicated credentialed <see cref="DnsManagementMachine"/> over the session's
    /// own connection (<paramref name="rpc"/> — transport.md: one WebSocket per
    /// actor; the page holds its own machine, this is the headless background
    /// writer), then every <c>auto_renew_poll_secs()</c>: the shared
    /// two-phase <c>AutoRenewScan()</c> (refresh + the at-risk ∧ auto-renew-on
    /// decision — one place all apps agree on *when*) then, if non-empty,
    /// <c>AutoRenewIssue(domains, bound_nest_id)</c> — the shared
    /// `fauna_client_dns::DnsManagementMachine` pair every hand-rolled
    /// per-app tick used to reimplement, now with no admin tap. Best-effort:
    /// a non-admin's admin-gated cert RPCs no-op (empty cert-status → an empty
    /// scan), and any tick failure is logged + retried next cycle. The first
    /// delay mirrors the other apps (sleep-then-tick).
    ///
    /// <para><b>Actor-scoped, not app-lifetime.</b> Until 2026-08-24 this loop was
    /// <c>while (true)</c> with no token and no guard, and its doc said "app
    /// lifetime; the process exit ends it" — which the in-process teardown refutes:
    /// windows never exits on an actor change and this is spawned at every
    /// transition into <c>Online</c>, so the app <b>accumulated one live cadence per
    /// login</b>, each still issuing TLS certs every tick through its own
    /// <c>FfiNestClient</c> against a nest that no longer knows that client
    /// (<c>account-scoping.md</c> § Isolation-contract gap ledger, the
    /// <c>windows (in-memory)</c> row). It now runs under an
    /// <see cref="Core.Services.ActorScope.BeginBackgroundLoop"/> lease and exits on
    /// the next actor change, disposing the machine it owns (it holds the session
    /// client's connection, so a leaked machine would keep that connection
    /// dialling).</para>
    ///
    /// <para><b>One per client and actor generation.</b> It is spawned from two
    /// post-auth sites: production's <c>StartMainAppAsync</c>, and the e2e login's
    /// <c>session</c> seam, which never reaches it and re-fires on every e2e session
    /// patch, often over the same client. A spawn that finds a live cadence on the
    /// same client in the same actor generation leaves at once
    /// (<see cref="_liveAutoRenewCadence"/>), so neither site stacks a second loop.</para>
    ///
    /// <para><b>The e2e poke.</b> In a Debug or <c>FAUNA_E2E_AGENT</c> build the wait
    /// is an <see cref="Core.Helpers.AutoRenewWake"/> minted for this loop, which the
    /// agent's <c>auto_renew_wake</c> ends early (convention 14). A Release build
    /// waits on the bare clock and carries no notifier (convention 15).</para>
    /// </summary>
    private static async Task RunAutoRenewCadenceAsync(Core.Services.INestRpcClient rpc)
    {
        // Taken FIRST, before anything can fail: a lease released by the `return`
        // below is the honest report that no cadence is live for this actor.
        using var lease = Core.Services.ActorScope.BeginBackgroundLoop();
        var actorGone = lease.Token;
        lock (_autoRenewCadenceGate)
        {
            if (_liveAutoRenewCadence is { } running
                && ReferenceEquals(running.Rpc, rpc)
                && !running.ActorGone.IsCancellationRequested)
            {
                return;
            }
            _liveAutoRenewCadence = (rpc, actorGone);
        }
        try
        {
            await RunClaimedAutoRenewCadenceAsync(rpc, actorGone);
        }
        finally
        {
            lock (_autoRenewCadenceGate)
            {
                // Only our own record: a successor's must outlive this loop's exit.
                if (_liveAutoRenewCadence is { } running
                    && ReferenceEquals(running.Rpc, rpc)
                    && running.ActorGone == actorGone)
                {
                    _liveAutoRenewCadence = null;
                }
            }
        }
    }

    private static readonly object _autoRenewCadenceGate = new();

    /// The client and actor-generation token of the live auto-renew cadence; null
    /// when none runs. Read and written under <see cref="_autoRenewCadenceGate"/>.
    private static (Core.Services.INestRpcClient Rpc, CancellationToken ActorGone)? _liveAutoRenewCadence;

    /// The body of <see cref="RunAutoRenewCadenceAsync"/>, once it holds the
    /// one-per-client claim.
    private static async Task RunClaimedAutoRenewCadenceAsync(
        Core.Services.INestRpcClient rpc, CancellationToken actorGone)
    {
        uniffi.fauna_client_dns.DnsManagementMachine machine;
        try
        {
            machine = await rpc.BuildDnsManagementMachineWithCredentialsAsync();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("AutoRenewCadence", $"could not start: {ex.Message}");
            return;
        }

        var pollInterval = TimeSpan.FromSeconds(
            uniffi.fauna_client_dns.FaunaClientDnsMethods.AutoRenewPollSecs());
#if DEBUG || FAUNA_E2E_AGENT
        // Minted once the machine is built, so a wake is refused until this loop can
        // run a pass. Retired in the finally below.
        var wake = Core.Helpers.AutoRenewWake.Mint(actorGone);
#endif
        try
        {
        while (!actorGone.IsCancellationRequested)
        {
#if DEBUG || FAUNA_E2E_AGENT
            await wake.WaitAsync(pollInterval, actorGone);
#else
            await Task.Delay(pollInterval, actorGone);
#endif
            try
            {
                var domains = await machine.AutoRenewScan();
                if (domains.Length == 0) continue;
                // The home nest the cert serves: the identity the connection is bound
                // to. A disagreeing claim makes BoundNestId() throw — no issuance, and
                // the tick-failed warning below logs it.
                using var linked = await rpc.BuildLinkedNestsMachineAsync();
                var boundNestId = await linked.BoundNestId();
                var pass = await machine.AutoRenewIssue(domains, boundNestId);
                foreach (var failure in pass.@failed)
                {
                    // One domain's issuance failure must not stop the rest —
                    // AutoRenewIssue already attempted every domain; this only logs.
                    ShellLog.Warn("AutoRenewCadence", $"issue {failure.@domain} failed: {failure.@error}");
                }
            }
            catch (Exception tx)
            {
                // A tick failure (non-admin's gated RPC, transient) — retry next cycle.
                ShellLog.Warn("AutoRenewCadence", $"tick failed: {tx.Message}");
            }
        }
        }
        catch (OperationCanceledException)
        {
            // The actor changed — the ONLY way out of the loop above, and not an
            // error. Fall through to the dispose below.
        }
        finally
        {
#if DEBUG || FAUNA_E2E_AGENT
            Core.Helpers.AutoRenewWake.Retire(wake);
#endif
            // This cadence owns the machine outright (nobody else has a handle), so
            // it must dispose it: the machine holds the session's FfiNestClient, and
            // a leaked holder keeps that client auto-reconnecting to the nest — see
            // DisposeNestClients for the same leak's other door.
            machine.Dispose();
        }
    }

    /// <summary>
    /// Route <paramref name="rpc"/>'s supervisor stop to the launch surface — the
    /// live signal of a mid-session supersession (<c>identity-succession.md</c>
    /// § Propagation → <i>Own device fleet</i>): the nest revokes the retired
    /// identity's bearers, the WS-RPC supervisor's re-mint is refused, and the
    /// connection-state pump reads the verdict off <c>FfiNestClient.SessionEndingVerdict</c>
    /// (<see cref="NestRpcClient.SessionEnding"/>). EVERY <see cref="_rpcClient"/> is
    /// built here, so none can miss the wiring — the e2e <c>set_state</c> login
    /// builds its own, and an unwired one there left the outcome-17 journey's
    /// superseded session stranded (measured 2026-09-27). A stop reported by a
    /// client this process has since replaced (a switch disposes the outgoing one)
    /// is ignored.
    /// </summary>
    private NestRpcClient NewRpcClient(string dialUrl, CryptoService crypto)
    {
        var rpc = new NestRpcClient(dialUrl, crypto, SyncNudge(), CredentialStore.Registry);
        rpc.SessionEnding += verdict => _ = OnSessionEndingAsync(rpc, verdict);
        return rpc;
    }

    private async Task OnSessionEndingAsync(NestRpcClient rpc, uniffi.fauna_ffi.FfiSessionEndingVerdict verdict)
    {
        try
        {
            if (!ReferenceEquals(rpc, _rpcClient))
            {
                ShellLog.Info("App", $"[post-auth] ignoring {verdict} from a replaced session");
                return;
            }
            await SessionEndingRoute.EscalateAsync(
                verdict, StolenCeremonyHold.Shared, () => EscalateIfStillLiveAsync(rpc));
        }
        catch (Exception ex)
        {
            ShellLog.Error("App", $"[post-auth] session-ending escalation failed: {ex.Message}");
        }
    }

    /// <summary>
    /// Perform the escalation — unless <paramref name="sessionRpc"/> is no longer the
    /// live session. A held-back escalation runs later, and by then the session it
    /// was owed for may already be gone (a switch, a sign-out, an earlier escalation):
    /// tearing down whatever replaced it would end a session nobody refused.
    /// </summary>
    private static Task EscalateIfStillLiveAsync(NestRpcClient? sessionRpc)
    {
        if (sessionRpc is null || !ReferenceEquals(sessionRpc, (Current as App)?._rpcClient))
        {
            ShellLog.Info("App", "[post-auth] escalation dropped: its session was already replaced");
            return Task.CompletedTask;
        }
        return EscalateToLaunchSurfaceHandler?.Invoke() ?? Task.CompletedTask;
    }

    /// <summary>
    /// C#-side TTL refresh loop — mirrors
    /// <c>libs/fauna-launch-machine/src/machine.rs::ttl_refresh_loop</c>, which
    /// is deliberately NOT exported via UniFFI (a Phase-2 concern; each app
    /// wraps it in platform-native scheduling for now — Linux calls the Rust
    /// loop directly since it's Rust, Windows replicates it here so its
    /// token-freshness behavior matches). Wakes
    /// <see cref="DirectNestClient.BearerRefreshBufferSecs"/> before the cached
    /// bearer's <c>expires_at</c> and asks the machine to refresh; re-arms on the
    /// new token's expiry; exits when the machine leaves <c>Online</c> (and isn't
    /// mid-refresh) or its token isn't <c>Valid</c>. That buffer matches
    /// <see cref="DirectNestClient"/>'s <c>EnsureAuthAsync</c> window (and
    /// Rust's <c>fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS</c>).
    ///
    /// A mid-session <c>NestIdentityChanged</c> verdict (security.md §
    /// Transport trust → § Post-auth surfacing) parks the machine in
    /// <see cref="LaunchPhase.IdentityChanged"/> rather than throwing — the
    /// loop's own phase check catches it on its NEXT read and re-enters
    /// <see cref="DispatchLaunchSnapshotAsync"/> on this SAME <c>machine</c>
    /// instance (never a synthesized one: <c>TrustNestIdentity()</c> reads
    /// secret + nest_url off the instance that produced the verdict and
    /// no-ops on any other), landing on the same blocking
    /// <see cref="Views.LaunchIdentityChangedPage"/> a fresh launch would.
    /// </summary>
    private async Task RunTtlRefreshLoopAsync(
        LaunchMachine machine, Frame rootFrame, ISessionAccount account, string secretHex, string baseUrl,
        Action? onBearerRefreshed = null)
    {
        // The session this loop serves — a supersession it finds escalates only
        // while that session is still the live one.
        var sessionRpc = _rpcClient;
        // Outer catch: last-resort net for a fire-and-forget task. If anything
        // outside the expected RefreshToken path throws (e.g. a UniFFI
        // PanicException from a poisoned mutex in Snapshot()), we at least log
        // before the task silently completes.
        try
        {
        while (true)
        {
            var snap = machine.Snapshot();
            switch (snap.@phase)
            {
                case LaunchPhase.Online:
                    break;
                case LaunchPhase.Refreshing:
                    // Transient mid-call state — wait it out and re-check.
                    await Task.Delay(TimeSpan.FromSeconds(1));
                    continue;
                case LaunchPhase.IdentityChanged:
                    // Route to the blocking surface on the SAME machine
                    // instance — see the method doc above. onOnboardingCompleted
                    // reuses the app-lifetime field: IdentityChanged's own
                    // Fallthrough is the only path here that can reach it (a
                    // mid-session "use a different nest" re-enters onboarding
                    // exactly like every other mid-session onboarding re-entry
                    // in this file).
                    await DispatchLaunchSnapshotAsync(
                        machine, snap, rootFrame, account, secretHex, baseUrl,
                        _onOnboardingCompleted ?? (() => { }));
                    return;
                case LaunchPhase.Offline when snap.@supersededSuccessor is not null:
                    // The refresh was refused as SUPERSEDED — the other door by which
                    // a mid-session succession reaches this device (the connection
                    // supervisor's stop is the first, NewRpcClient). Same route:
                    // held back while this device's own ceremony owns Account.
                    await SessionEndingRoute.EscalateAsync(
                        uniffi.fauna_ffi.FfiSessionEndingVerdict.Superseded, StolenCeremonyHold.Shared,
                        () => EscalateIfStillLiveAsync(sessionRpc));
                    return;
                case LaunchPhase.Offline when snap.@signInRefused:
                    // The refresh was refused `fauna.auth.not_registered` (the user
                    // was suspended while signed in) — the TTL loop's own door to
                    // the sign-in-refused surface; the connection supervisor's stop
                    // (NewRpcClient) is the other. Same route and same ceremony hold.
                    await SessionEndingRoute.EscalateAsync(
                        uniffi.fauna_ffi.FfiSessionEndingVerdict.SignInRefused, StolenCeremonyHold.Shared,
                        () => EscalateIfStillLiveAsync(sessionRpc));
                    return;
                case LaunchPhase.Offline when snap.@lockedUntilSecs is not null:
                    // The post-auth silent challenge was refused as LOCKED
                    // (`fauna.auth.account_locked` — devices.md § The locked state):
                    // locked while signed in, by this account's own Sessions page on
                    // another device or by whoever holds its key. Re-enter launch,
                    // whose re-run challenge meets the lock and paints its notice —
                    // tui's SilentSignInVerdict::Locked arm, FaunaKit's
                    // performPostAuthSilentSignIn `.AccountLocked` arm.
                    ShellLog.Warn("App", "[post-auth] this account is locked — re-entering launch");
                    await EscalateIfStillLiveAsync(sessionRpc);
                    return;
                default:
                    // Not Online and not mid-refresh → the loop's purpose is over.
                    return;
            }

            if (snap.@token is not TokenStatus.Valid v)
                // Online but the token isn't Valid — only test setups; exit cleanly.
                return;

            var expiresAt = v.@expiresAtSecs;
            var nowSecs = (ulong)DateTimeOffset.UtcNow.ToUnixTimeSeconds();
            var sleepSecs = expiresAt > nowSecs + DirectNestClient.BearerRefreshBufferSecs
                ? expiresAt - DirectNestClient.BearerRefreshBufferSecs - nowSecs
                : 0UL;
            if (sleepSecs > 0)
                await Task.Delay(TimeSpan.FromSeconds(sleepSecs));

            // Re-check after the sleep — state may have changed; if expires_at
            // shifted, someone else already refreshed.
            if (machine.Snapshot().@token is TokenStatus.Valid v2)
            {
                if (v2.@expiresAtSecs != expiresAt)
                    continue;  // different expiry — another path refreshed; re-compute
                // same token still cached — refresh it below
            }
            else
            {
                return;
            }

            try
            {
                await machine.RefreshToken();

                // Wake the hydration session loop so the agent gets the
                // freshly-minted bearer now rather than at its next tick (the loop
                // owns all pipe pushes — RefreshBearer and, when the agent reports
                // no capability, the full re-provision). on-demand-files.md § On-Demand
                // Files; windows.md § On-demand hydration host.
                onBearerRefreshed?.Invoke();
            }
            catch (Exception ex)
            {
                // The failed refresh moved state to Offline; the next
                // iteration's snapshot read detects it and returns.
                System.Diagnostics.Debug.WriteLine($"[ttl-refresh] RefreshToken failed: {ex.Message}");
                ShellLog.Error("App", $"[ttl-refresh] RefreshToken failed: {ex.Message}");
            }
        }
        }
        catch (Exception ex)
        {
            // Last-resort net: log and let the fire-and-forget task complete
            // gracefully rather than silently swallowing an unexpected fault.
            System.Diagnostics.Debug.WriteLine($"[ttl-refresh] loop exiting on error: {ex.Message}");
            ShellLog.Error("App", $"[ttl-refresh] loop exiting on error: {ex.Message}");
        }
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// E2E-only: on-demand trigger for the SAME post-auth identity re-check
    /// <see cref="RunTtlRefreshLoopAsync"/> already runs on its own near-expiry
    /// schedule — exposed to the bridge (<c>TestAgent.cs</c>'s <c>silent_sign_in</c>
    /// command) so <c>test_nest_identity_pin_post_auth.py</c> can force the check
    /// immediately after seeding a bogus pin, rather than waiting for the loop's own
    /// wakeup (security.md § Post-auth surfacing). Mirrors linux's <c>FaunaClient::silent_sign_in</c> / apple's
    /// <c>performPostAuthSilentSignIn</c> — all three drive the SAME production
    /// verdict path (<c>fauna.auth.handshake</c> → <c>classify_silent_challenge</c> in
    /// the Rust `refresh_internal`), never a shortcut that fakes the verdict.
    ///
    /// <c>machine.RefreshToken()</c> is the identical call the loop's own tick makes
    /// on the SAME machine instance <see cref="_liveTtlRefreshContext"/> holds; a mid-
    /// session <c>NestIdentityChanged</c> verdict parks that instance in
    /// <see cref="LaunchPhase.IdentityChanged"/>, which this then dispatches to
    /// <see cref="Views.LaunchIdentityChangedPage"/> exactly as the loop's own next
    /// tick would — this just doesn't wait for that tick.
    ///
    /// Returns <c>false</c> with no other effect when no live session context is
    /// held (post-logout, or never authenticated) — the caller fails loudly on that
    /// (testing.md convention 11: a test command must fail loudly, never silently
    /// drop).
    /// </summary>
    private async Task<bool> RunSilentSignInForTestAsync()
    {
        if (_liveTtlRefreshContext is not { } ctx) return false;
        var sessionRpc = _rpcClient;
        try
        {
            await ctx.Machine.RefreshToken();
        }
        catch (uniffi.fauna_launch_machine.LaunchException.AccountLocked)
        {
            // Parked below on the snapshot's side channel, as the loop reads it.
        }
        var snap = ctx.Machine.Snapshot();
        if (snap.@phase is LaunchPhase.IdentityChanged)
        {
            await DispatchLaunchSnapshotAsync(
                ctx.Machine, snap, ctx.RootFrame, ctx.Account, ctx.SecretHex, ctx.BaseUrl,
                _onOnboardingCompleted ?? (() => { }));
        }
        else if (snap.@lockedUntilSecs is not null)
        {
            // The loop's own Locked arm, forced now (RunTtlRefreshLoopAsync).
            await EscalateIfStillLiveAsync(sessionRpc);
        }
        return true;
    }

    /// <summary>Static forwarder to <see cref="RunSilentSignInForTestAsync"/> on the
    /// live app instance — the <see cref="CurrentRpc"/>-style accessor TestAgent.cs
    /// calls, so it never touches <c>Application.Current</c> directly. <c>false</c>
    /// (never a thrown <c>NullReferenceException</c>) when the app isn't up yet.</summary>
    internal static Task<bool> TriggerSilentSignInForTestAsync() =>
        (Current as App)?.RunSilentSignInForTestAsync() ?? Task.FromResult(false);
#endif

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// E2E-only: build the real shared-Rust <c>ConversationsSession</c> off the
    /// just-authenticated WS-RPC connection in the <c>set_state</c> login path (which
    /// never reaches <see cref="StartMainAppAsync"/>, where production builds it).
    /// The session itself is built for EVERY e2e login — the nav actions hand it to
    /// the pages via <c>ServiceClients.ConvSession</c>, and without it every
    /// <c>_convSession</c>-gated folder gesture (share / leave / serve / paywall)
    /// silently no-ops. Mirrors linux <c>conv_backend::start_conversations_session</c>,
    /// which also runs the real session under e2e for every login.
    ///
    /// The RECEIVE-LOOP half stays gated on <c>FAUNA_E2E_REAL_CONVERSATIONS</c>: the
    /// default e2e path keeps the mock <c>ConversationsManagerHost</c> (so the
    /// <c>conversations_inject_inbound</c> DM tests stay deterministic), while the
    /// conversations/mail tier_3 tests that must exercise the REAL loop set the flag so
    /// the snapshot reflects real-decrypted mail. Building the session alone is
    /// side-effect-light — it opens the MLS store; key-package replenish is
    /// session-owned inside <c>StartReceiveLoop</c>, so the gate still decides whether
    /// this client publishes any.
    ///
    /// Best-effort: a failure yields null and the login proceeds exactly as before the
    /// session was threaded (the page falls back to the mock manager). No
    /// <c>ConfigureAwait(false)</c> (mirrors the production login block at the
    /// <c>StartReceiveLoop</c> call). Nothing here mutates WinUI bound state —
    /// <c>MarkRealRailsRegistered</c> flips a static bool and the receive loop runs
    /// in Rust — so the kickoff is safe even though it originates off the UI thread
    /// (the set_state command handler), unlike the production login block.
    /// </summary>
    private async Task<uniffi.fauna_conversations.ConversationsSession?> BuildE2eConvSessionAsync(
        NestRpcClient rpc, string? deviceIdHex)
    {
        try
        {
            // Build OVER ConversationsManagerHost.Instance — the same manager any
            // conversations_inject_* command may already have written into (or will
            // write into later via the mock rail) — so the real rails register onto
            // it rather than replacing it. Nothing ever needs to be swapped in
            // afterwards (the old RegisterRealManager call this replaced could
            // orphan anything injected before the session finished building; see
            // testing.md § Cross-app e2e conventions, convention 10).
            Core.Logs.E2eTrace.Write("[conv-build] resolving ConversationsManagerHost.Instance");
            var convHost = FaunaApp.Conversations.ConversationsManagerHost.Instance;
            Core.Logs.E2eTrace.Write("[conv-build] host resolved; calling BuildConversationsSessionAsync");
            var convSession = await rpc.BuildConversationsSessionAsync(convHost, deviceIdHex: deviceIdHex);
            Core.Logs.E2eTrace.Write("[conv-build] BuildConversationsSessionAsync returned");
            if (convSession is null)
            {
                ShellLog.Error("e2e-conv", "BuildConversationsSessionAsync returned null");
                return null;
            }
            // Release-before-build: this method is called both from a fresh launch
            // (StartMainAppAsync's seeded-e2e branch) and from a live e2e re-login
            // (the `session` WS-RPC command's kickoff, below) — either can already
            // hold a predecessor here without an intervening DisposeNestClients()
            // (account-runtime.md § Multi-instance concurrency). One seam for both callers, mirroring corollary 2.
            RetireConvSession(null, _e2eRealConvSession);
            // Hold the Arc alive so the receive loop's liveness Weak stays upgradeable.
            _e2eRealConvSession = convSession;
            _e2eRealConvDeviceIdHex = deviceIdHex;

            // Phase two's archive links, as the production login does (every login
            // wires them, as linux's conv_backend does): the factory built them with
            // this session; the host drives them and repaints the Contacts rows.
            await StartArchiveLinksAsync(rpc);
            // …and the Archive rail's first read, as the production login does.
            _ = RefreshArchiveThreadsAsync(convSession);

            // The DM OS-toast observer the production login block attaches — the
            // witness for conversations outcome 11 reads what it fires, and the seeded
            // / `session` e2e login never reaches that block, so without this the e2e
            // app fires nothing and no banner tick ever runs. Outside the
            // FAUNA_E2E_REAL_CONVERSATIONS gate below on purpose: the default
            // (mock-manager) path is where the `conversations_inject_*` DM tests live.
            // This runs OFF the UI thread (the set_state handler), so hand the observer
            // the window's queue — its own thread has none, and a null queue would diff
            // inline on the mutator's thread. The toast host is initialised here for the
            // same reason: production does it in StartMainAppAsync, which this login
            // never reaches, so without it every toast would be declined (idempotent).
            NotificationService.Initialize();
            // This builder always makes a FRESH session over a fresh manager (the
            // predecessor was retired just above), so an observer the field still
            // holds is bound to the discarded one — the e2e re-login reaches
            // DropActorScopedState without any of the arms that null it (factory
            // reset, sign-out, switch). Left set, AttachMessageToastObserver's
            // re-entry guard would skip the new manager and no banner would ever
            // fire for the second journey in a reused app.
            _messageToastObserver = null;
            AttachMessageToastObserver(convSession.Manager(), _window?.DispatcherQueue);

            // The room-post key seam's SECOND meeting point (ui/feed.md § Encryption at
            // rest → Room-restricted — the app half): FeedPage.Page_Loaded installs it
            // when a session already exists, but a room-seat e2e journey navigates
            // straight to `feed` (`room_seats.py`'s launch), which can build the feed
            // manager BEFORE this session finishes building — leaving it with no seam to
            // find there. Backfill onto whatever manager already exists; a fresh
            // RefreshOwnRooms alongside it, since the manager's own initial hydrate (which
            // would otherwise pick this up) already ran with no seam installed.
            if (Core.FeedManagerHost.Current is { } feedManager)
            {
                feedManager.SetRoomPostKeys(convSession);
                _ = FaunaApp.Conversations.RoomsRefreshObserver.RefreshAsync(feedManager);
            }

            if (!string.IsNullOrEmpty(Environment.GetEnvironmentVariable("FAUNA_E2E_REAL_CONVERSATIONS")))
            {
                var mgr = convSession.Manager();
                // Readiness flag only now — mgr IS ConversationsManagerHost.Instance
                // (built over it above), so there is nothing left to publish.
                FaunaApp.Conversations.ConversationsManagerHost.MarkRealRailsRegistered();
                // (Keypackage replenish is session-owned inside StartReceiveLoop, after
                // the MLS replica restore — see the StartMainAppAsync twin above.)
                try { await convSession.StartReceiveLoop(); }
                catch (Exception ex) { ShellLog.Error("e2e-conv", $"StartReceiveLoop threw: {ex.GetType().Name}: {ex.Message}"); }

                // Draft-persistence v2, conversations leg (file-sync.md § Drafts Sync;
                // conversations.md § Persistence): wire the nest-backed __drafts autosync into
                // the e2e real-conversations harness. The production build lives in
                // StartMainAppAsync, inside the !FAUNA_E2E_BRIDGE block the e2e never reaches,
                // so windows had NO drafts under e2e — this is the windows twin of apple's
                // ConversationsVM.attachDraftsSync(applySessionPatch).
                await WireConversationDraftsAsync(rpc, mgr);
            }
            return convSession;
        }
        catch (Exception ex)
        {
            ShellLog.Error("e2e-conv", $"BuildE2eConvSessionAsync threw: {ex.GetType().Name}: {ex.Message}");
            return null; /* optional — the page still renders the mock */
        }
    }

    /// <summary>
    /// Await the e2e login's <see cref="BuildE2eConvSessionAsync"/>, BOUNDED. A nest that
    /// is absent or hangs must never stall the navigation: on timeout (or no build in
    /// flight) this yields null, which is exactly the pre-threading behavior — the page
    /// navigates with a null <c>ConvSession</c>. Against a live nest the build is a WS
    /// connect + identity read + MLS-store open, well inside the bound.
    /// </summary>
    private async Task<uniffi.fauna_conversations.ConversationsSession?> AwaitE2eConvSessionAsync()
    {
        var task = _e2eConvSessionTask;
        if (task is null)
        {
            return null;
        }
        var finished = await Task.WhenAny(task, Task.Delay(TimeSpan.FromSeconds(10)));
        return finished == task ? await task : null;
    }

    /// <summary>
    /// Await the e2e login's <see cref="WireFeedDraftsAsync"/> kickoff, BOUNDED —
    /// same shape as <see cref="AwaitE2eConvSessionAsync"/>. A nest that is absent
    /// or hangs must never stall navigation: on timeout (or no build in flight)
    /// this just returns, which is exactly the pre-threading behavior — the page
    /// navigates with <see cref="FeedDraftsSync"/> possibly still null, and
    /// FeedPage.Page_Loaded's own null-check degrades gracefully (no draft
    /// persistence for that navigation, nothing worse).
    /// </summary>
    private async Task AwaitE2eFeedDraftsSyncAsync()
    {
        var task = _e2eFeedDraftsSyncTask;
        if (task is null) return;
        await Task.WhenAny(task, Task.Delay(TimeSpan.FromSeconds(10)));
    }
#endif

    /// <summary>
    /// Attach the app-lifetime DM OS-toast observer to <paramref name="mgr"/>
    /// (conversations.md § Where logic lives — OS notifications are client glue:
    /// diff snapshots, call the native toast API). Deliberately NOT page-scoped
    /// (unlike ConversationsPage's ConversationsNotifyObserver): toasts must fire
    /// even when the conversations page is closed. Held in a field so the UniFFI
    /// callback isn't GC'd for the manager's lifetime. Ungated: the production login
    /// block calls it; the e2e session builder calls it too, so the witness sees the
    /// same wiring rather than a test-only twin.
    ///
    /// Null-guarded: every caller that resets ConversationsManagerHost.Instance
    /// to a fresh manager also nulls this field first (DropActorScopedState's
    /// callers — factory-reset, sign-out, switch), so a null here always means
    /// "no observer on the CURRENT manager yet". A same-actor re-entry that does
    /// NOT reset the manager (IdentityChanged → Trust) leaves the field non-null
    /// and <paramref name="mgr"/> unchanged — re-registering there would attach a
    /// SECOND observer to the same long-lived manager, firing the OS toast once per
    /// accumulated registration for every later message.
    /// There is no manager.RemoveObserver to retire the old one individually, and
    /// ClearObservers() would also silently drop ConversationsPage's own page-scoped
    /// observer if the page happened to be open across the re-entry — so skipping
    /// re-registration (the existing observer is still valid; it is bound to this
    /// same unchanged manager) is the only fix that cannot regress a sibling observer.
    ///
    /// <paramref name="dispatcher"/> is the UI-thread queue the observer marshals its
    /// diff through; null takes the calling thread's, right only for a caller already
    /// on the UI thread (the production login block). The e2e session builder is not.
    /// </summary>
    private void AttachMessageToastObserver(
        uniffi.fauna_conversations.ConversationsManager mgr,
        Microsoft.UI.Dispatching.DispatcherQueue? dispatcher = null)
    {
        if (_messageToastObserver is not null) return;
        _messageToastObserver = new FaunaApp.Conversations.MessageToastObserver(mgr, dispatcher);
        mgr.AddObserver(_messageToastObserver);
    }

    /// <summary>
    /// Build the nest-backed <c>__drafts</c> autosync (the shared <c>DraftsSync</c> wrapper)
    /// over this session's conversations manager, expose it as <see cref="ConvDrafts"/>, and
    /// start restoring any stored conversation drafts INTO the manager — not awaited, since the
    /// shared load retries a lost read for minutes; the restore fills the composer whenever it
    /// lands. Shared by the production login
    /// (<see cref="StartMainAppAsync"/>) and the e2e real-conversations harness so both wire
    /// drafts identically (file-sync.md § Drafts Sync; conversations.md § Persistence).
    /// Best-effort — a transient build/restore failure must never block login.
    /// </summary>
    private static async Task WireConversationDraftsAsync(
        NestRpcClient rpc, uniffi.fauna_conversations.ConversationsManager mgr)
    {
        try
        {
            var draftsSync = await rpc.BuildDraftsSyncAsync("conversations");
            var convDrafts = new ConversationDraftsService(draftsSync, new ManagerDraftStore(mgr));
            ConvDrafts = convDrafts;
            // Not awaited: the shared DraftsSync retries a launch read lost to the
            // transport for minutes (conversation-drafts.md § Persistence, *A launch
            // load lost to the transport is retried*), and login must not wait on it.
            // A late restore is safe — it fills and never overwrites live typing, and
            // RestoreOnLaunchAsync swallows its own failure.
            _ = convDrafts.RestoreOnLaunchAsync();
        }
        catch { /* drafts are best-effort */ }
    }

    /// <summary>
    /// Build the nest-backed <c>__drafts</c> autosync (rail <c>"posts"</c>) and expose
    /// it as <see cref="FeedDraftsSync"/>. Unlike <see cref="WireConversationDraftsAsync"/>
    /// this builds ONLY the sync handle, never a <see cref="FeedDraftsService"/> —
    /// <c>FfiFeedManager</c> does not exist yet at login (it is built lazily on first
    /// Feed-page visit), so <c>FeedPage.Page_Loaded</c> pairs this handle with the
    /// session's manager itself, once. Shared by the production login
    /// (<see cref="StartMainAppAsync"/>) and the e2e login handler so both wire feed
    /// drafts identically. Best-effort — a transient build failure must never block
    /// login.
    /// </summary>
    private static async Task WireFeedDraftsAsync(NestRpcClient rpc)
    {
        Core.Logs.E2eTrace.Write("[feed-drafts] WireFeedDraftsAsync starting");
        try
        {
            FeedDraftsSync = await rpc.BuildDraftsSyncAsync("posts");
            Core.Logs.E2eTrace.Write("[feed-drafts] WireFeedDraftsAsync succeeded, FeedDraftsSync set");
        }
        catch (Exception ex)
        {
            Core.Logs.E2eTrace.Write($"[feed-drafts] WireFeedDraftsAsync threw: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Build the nest-backed events-rail autosync (typed, not raw bytes — see
    /// <see cref="EventDraftsService"/>'s own doc comment for why) and expose it as
    /// <see cref="EventDraftsSync"/>. Mirrors <see cref="WireFeedDraftsAsync"/> exactly:
    /// builds ONLY the sync handle, never the full <see cref="EventDraftsService"/> —
    /// <c>EventsPage</c> does not exist yet at login (built lazily on first Events-page
    /// visit), so <c>EventsPage</c>'s own first load pairs this handle with its compose
    /// surface. Best-effort — a transient build failure must never block login.
    /// </summary>
    private static async Task WireEventDraftsAsync(NestRpcClient rpc)
    {
        Core.Logs.E2eTrace.Write("[event-drafts] WireEventDraftsAsync starting");
        try
        {
            EventDraftsSync = await rpc.BuildEventDraftsSyncAsync();
            Core.Logs.E2eTrace.Write("[event-drafts] WireEventDraftsAsync succeeded, EventDraftsSync set");
        }
        catch (Exception ex)
        {
            Core.Logs.E2eTrace.Write($"[event-drafts] WireEventDraftsAsync threw: {ex.GetType().Name}: {ex.Message}");
        }
    }

#if DEBUG || FAUNA_E2E_AGENT
    // ── The e2e state protocol + command table (testing.md convention 15) ──
    // SerializeState and HandleTestCommand are the two closures the TestAgent is
    // Configure()d with, and it is their only caller. They are the app's entire
    // scripted-control surface: HandleTestCommand can sign in, reset credentials,
    // navigate, and drive `*ForTest` UniFFI seams. Absent from a Release build.
    private Dictionary<string, object?> SerializeState(string? secretHex, string? nestUrl, ISessionAccount account)
    {
        // Read real data from AppDataSnapshot (populated by ViewModels after load).
        // null = section not implemented; [] = implemented but empty.
        var feedPosts = Services.AppDataSnapshot.GetFeedPostsForState();
        var notifications = Services.AppDataSnapshot.GetNotificationsForState();
        var conversations = Services.AppDataSnapshot.GetConversationsForState();
        var conversationThreads = Services.AppDataSnapshot.GetConversationsThreadsForState();
        var conversationSort = Services.AppDataSnapshot.GetConversationSortForState();
        var contacts = Services.AppDataSnapshot.GetContactsForState();
        var knocks = Services.AppDataSnapshot.GetKnocksForState();
        var events = Services.AppDataSnapshot.GetEventsForState();
        var sync = Services.AppDataSnapshot.GetSyncForState();

        var state = new Dictionary<string, object?>
        {
            ["session"] = new Dictionary<string, object?>
            {
                // e2e-conventions.md § convention 11 → *what `session.authenticated`
                // means*: an explicit set_state override wins; otherwise "the
                // authenticated main app is mounted". STORED CREDENTIALS ARE NOT A
                // SESSION — this used to read `_cryptoService?.HasKey == true`, so a
                // credentialed relaunch that the launch machine correctly routed to
                // the wizard (verify 404 → invite_request/claim_code, or an
                // awaiting-manual-dns "Almost ready") still reported `true`. Since
                // `launch_harness.reached_authenticated_app()` uses this flag as the
                // arrival signal for every native app, that made any windows test
                // resting on it go green BEFORE the app had mounted anything.
                ["authenticated"] = _testAuthenticatedOverride is bool authOverride
                    ? authOverride
                    : _mainAppMounted,
                ["node_url"] = nestUrl ?? account.NestUrl,
                ["secret_hex"] = secretHex ?? account.SecretHex,
                ["handle"] = _handle,
                ["device_id"] = _deviceId ?? account.StoredDeviceId,
                ["actor_id"] = _cryptoService?.HasKey == true ? _cryptoService.ActorIdHex : null,
            },
            ["nav"] = new Dictionary<string, object?>
            {
                ["stack"] = new[] { new Dictionary<string, object?> {
                    ["view"] = _testCurrentView
                } },
                ["modal"] = (object?)null,
            },
            ["settings"] = new Dictionary<string, object?>
            {
                ["inbox_mode"] = _testInboxMode,
                // The token id behind each painted session-card, in order — null
                // before a visit's first hydrate (actions/sessions.py::painted_token_ids;
                // tui/android/apple publish the same key).
                ["session_token_ids"] = Core.ViewModels.SessionsViewModel.PaintedTokenIdsForTest,
            },
            // The held session bearer's schedule and this app's own session ids
            // (`fauna_e2e_agent::LAUNCH_TOKEN_KEY`), TOP-level like the other
            // apps'. On the UniFFI apps the bearer is this client's
            // WsChallengeBearer; shared Rust builds the value.
            ["launch_token"] = LaunchTokenForSerialization(),
            // The `barrier` self-test's only observables — TOP-level, not under
            // `data`, because the cross-app test reads `get_state("barrier_probe")`
            // and tui/linux/web publish them at the same depth
            // (`fauna_e2e_agent::BARRIER`).
            ["barrier_probe"] = _barrierProbe,
            // What the barrier saw at its OWN ack, frozen — the only key the
            // self-test asserts (`fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`).
            ["barrier_ack_probe"] = _barrierAckProbe,
            // Convention 14's negative-assert pair, same top-level depth as
            // tui/linux/web. Both are LIVE reads, deliberately — unlike the frozen
            // ack probe above, a monotonic counter is conservative under a late
            // read (it can only reveal MORE teardowns), so settling makes the
            // assertion stricter rather than vacuous
            // (`fauna_e2e_agent::SESSION_GENERATION_KEY` argues this out).
            ["session_generation"] = Core.Services.E2eSessionCounters.SessionGeneration,
            // The completion observable that pair needs on an app whose re-auth
            // prompt is a native OS dialog with no test ID
            // (`fauna_e2e_agent::ACTIVATION_GESTURES_KEY`).
            ["activation_gestures"] = Core.Services.E2eSessionCounters.ActivationGestures,
            // Every delivered launch verdict, rendered or dropped — the barrier
            // test_launch_superseded_verdict.py's negative assert rests on, at the
            // same top-level depth as tui/linux/web/android/apple
            // (`fauna_e2e_agent::LAUNCH_VERDICTS_KEY`). Counted in DriveLaunchAsync,
            // the one seam every asynchronous launch drive passes.
            ["launch_verdicts"] = Core.Services.LaunchCurrency.Shared.Observable,
            // Convention 14's other sweep-side causal barrier — same top-level
            // depth as tui/linux/web/android, a two-getter read over the
            // process-wide registry (`fauna_e2e_agent::ALERT_SWEEP_PASSES_KEY`).
            // Do NOT invent a windows-side counter: all counting is shared Rust.
            ["alert_sweep_passes"] = new Dictionary<string, object?>
            {
                ["started"] = (long)Core.Services.CriticalAlertsHost.Instance.SweepPassesStarted(),
                ["completed"] = (long)Core.Services.CriticalAlertsHost.Instance.SweepPassesCompleted(),
            },
            // Convention 14's `mls_folded_commits` observable, also TOP-level —
            // same depth as `session_generation`/`alert_sweep_passes` above, and
            // the depth `helpers/waiting.py` reads
            // (`fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY`;
            // `docs/goal/architecture/e2e-latency-independent-assertions.md` § D8).
            // No session yet publishes `{}`, never omits the key — the shared-Rust
            // contract's own pre-session arm
            // (`fauna_conversations::state_json::mls_folded_commits_json_for_session`),
            // deliberately NOT apple's omit-the-key deviation: windows HAS this
            // leg, so `null`/absent must stay reserved for an app that doesn't.
            ["mls_folded_commits"] = MlsFoldedCommitsForSerialization(),
            // Every chat channel this device has bound a thread to — the
            // `mls_folded_commits` twin, TOP-level at the same depth
            // (`fauna_e2e_agent::MLS_BOUND_CHANNELS_KEY`, which owns the
            // contract; `helpers/waiting.py::await_channel_bound` reads it). A
            // Welcome-only direct thread is bound but listed on no rail, so a
            // second device's restore is witnessed here, not in
            // `data.conversation_threads`. No session yet publishes `[]`, never
            // omits the key: an absent key is refused loudly by the wait.
            ["mls_bound_channels"] = MlsBoundChannelsForSerialization(),
            // Convention 14's `conv_receive_cycles` observable, also TOP-level —
            // same depth as `mls_folded_commits` above, and the depth
            // `helpers/waiting.py` reads (`fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY`;
            // `docs/goal/architecture/e2e-latency-independent-assertions.md` § D9).
            // No session yet publishes `{"started": 0, "completed": 0, "exit":
            // null}` — the shared-Rust contract's own pre-session arm
            // (`fauna_conversations::state_json::conv_receive_cycles_json`'s
            // `None` branch) — NOT `mls_folded_commits`'s `{}` shape, which is a
            // per-channel map with no fixed keys.
            ["conv_receive_cycles"] = ConvReceiveCyclesForSerialization(),
            // Convention 14's `account_pump_cycles` observable, same top-level
            // depth as `conv_receive_cycles` above — the W3 account plane's
            // twin (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`;
            // `account-data-plane.md` § The account store → *The client-side
            // lifecycle*). No connected client yet publishes the shared
            // contract's own pre-session shape
            // (`{"started": 0, "completed": 0, "runtime": false, "holder":
            // false}`) rather than omitting the key — omission means "this
            // app has no leg", and windows has one.
            ["account_pump_cycles"] = AccountPumpCyclesForSerialization(),
            // Convention 14's `feed_reloads` observable, same top-level depth as
            // `account_pump_cycles` above (`fauna_e2e_agent::FEED_RELOADS_KEY`;
            // `e2e-latency-independent-assertions.md` § Implementation status
            // today — windows was the last app entrusted this leg). No live
            // FfiFeedManager yet returns the shared contract's own pre-manager
            // shape (`fauna_feed::feed_reloads_json(None)`:
            // `{"started": 0, "completed": 0, "committed_gen": 0}`), never
            // omits the key — windows HAS this leg.
            ["feed_reloads"] = FeedReloadsForSerialization(),
            // The Devices/Folders refresh barrier, `feed_reloads`' twin
            // (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`;
            // `e2e-latency-independent-assertions.md` § Implementation status
            // today): the zero triple before the session machine exists, never an
            // omitted key — windows HAS this leg.
            ["devices_refreshes"] = DevicesRefreshesForSerialization(),
            // Convention 14's `serving_enablement` observable, same top-level depth as
            // `feed_reloads` above (`fauna_e2e_agent::SERVING_ENABLEMENT_KEY`;
            // `e2e-latency-independent-assertions.md` § Implementation status
            // today) — the post-claim serving-enablement step's per-actor run
            // records, which the calendar-onboarding witnesses anchor on instead
            // of a settle window. Published unconditionally from launch: before
            // the `LoggedIn` handoff it is the shared contract's own empty shape
            // (zero counts, no runs) — the legitimate "not run yet", never an
            // omitted key, which would read as "this app has no leg" and windows
            // HAS one.
            ["serving_enablement"] = ServingEnablementForSerialization(),
            // What this process's custody ceremony sink did with each received
            // offer / accept / deliver (`fauna_e2e_agent::CUSTODY_CEREMONY_INGEST_KEY`),
            // top-level like tui's and linux's — devices outcome 15's barrier for the
            // owner's send-again. Five atomic loads on the FFI side (convention 11's
            // corollary); null before a client exists (pre-auth), never zeros.
            ["custody_ceremony_ingest"] = CustodyCeremonyIngestForSerialization(),
#if P2P_SHARE
            // What this process's share plane has served to peers
            // (`fauna_e2e_agent::SHARE_SERVE_TALLY_KEY`), top-level like tui's — the
            // witness for the offline-sharing resume/serve journeys. A plain
            // lock-and-clone on the FFI side (convention 11's corollary), legal on
            // the state path.
            ["share_serve_tally"] = ShareServeTallyForSerialization(),
#endif
            // The new-message OS banners this process actually fired, plus the diff-tick
            // counter pair that makes a negative read of them sound — conversations
            // outcome 11's only observable (`fauna_e2e_agent::MESSAGE_BANNERS_KEY`), same
            // top-level depth as tui/linux/web/apple. `MessageToastObserver` feeds it.
            ["message_banners"] = MessageBannersForSerialization(),
            // The cross-app connection barrier's observable
            // (`fauna_e2e_agent::CONNECTION_KEY`), same top-level depth as
            // tui/linux/web/android/apple — `{"state": <lowercase wire word>,
            // "online": <bool>}`. The word comes from `OfflineGate.Shared`, the
            // one place windows turns an `FfiConnectionState` into the lowercase
            // wire word (`MainViewModel.OnConnectionStateChanged`), so the
            // connection-status indicator a user reads and this observable can
            // never disagree; the boolean is the shared-Rust
            // `connection_is_online` UniFFI door, never `state == "connected"`
            // (the gate's polarity is the opposite: online unless a KNOWN
            // offline word). `OfflineGate.Shared.ConnectionState` starts
            // `"disconnected"` before any session and is never omitted — like
            // windows' other top-level observables above, deliberately NOT
            // apple's omit-pre-session shape.
            ["connection"] = ConnectionForSerialization(),
            // The launch clock's e2e offset and the launch machine's `now`
            // (`fauna_e2e_agent::CLOCK_KEY`), same top-level depth and
            // `{"offset_secs", "now_secs"}` shape as tui/linux/web/android/apple —
            // the in-app control the wrong-clock launch witness
            // (`test_smoke_l_wrong_clock_still_signs_in`) reads to prove the
            // `FAUNA_E2E_CLOCK_OFFSET_SECS` seed reached the process that signed in.
            // Both numbers come from the launch machine's own clock through the
            // `test-helpers` UniFFI getters; they are process-global, so there is
            // no pre-session arm and the key is always published.
            ["clock"] = ClockForSerialization(),
            // Every connection-state value the indicator received, repeats
            // included — `{"reports", "transitions", "word"}`, the stickiness
            // proof (`fauna_e2e_agent::CONNECTION_REPORTS_KEY`). Fed by
            // `MainViewModel.OnConnectionStateChanged`, the same handler that
            // paints the indicator; counted by shared Rust.
            ["connection_reports"] = DecodeObservable(Core.Services.E2eLoudSurfaces.ConnectionReportsJson()),
            // Every error surface a painted frame showed —
            // `{"count", "showing"}` (`fauna_e2e_agent::PAINTED_ERRORS_KEY`). Fed
            // by `Testing.PaintedErrorObserver`; counted by shared Rust.
            ["painted_errors"] = DecodeObservable(Core.Services.E2eLoudSurfaces.PaintedErrorsJson()),
            // Convention 17's `painted-text-has-area` field
            // (`helpers/frame_invariants.py::_check_painted_text_has_area` owns the
            // contract): `{"starved": [{"id", "text", "width", "height", "pane",
            // "depth"}]}`, every painted, id-bearing text element of the last
            // laid-out frame whose box is under one DIP. Fed by
            // `Testing.PaintedTextAreaObserver`; a reference read, as this ack path
            // requires.
            ["painted_text_area"] = Testing.PaintedTextAreaObserver.State(),
            // Every knock push the knock-toast arm received and every toast it
            // showed — `{"seen", "fired": [{"key", "text", "replace_id"}]}`
            // (`fauna_e2e_agent::KNOCK_NOTICES_KEY`, notifications outcome 15).
            // Fed by `NotificationService.ShowKnockNotification`; logged by shared Rust.
            ["knock_notices"] = DecodeObservable(Core.Services.E2eLoudSurfaces.KnockNoticesJson()),
            // The switcher's "open in new window" spawns (account-scoping.md
            // § Concurrent instances → the running instance's surface). Cross-app
            // shape — apple's InstanceSpawner.stateRecords, linux's
            // instance_remote::spawned_instances — so one test reads every platform.
            ["spawned_instances"] = Services.InstanceSpawner.StateRecords(),
            // Process-health counters a test can assert on directly, so a resource
            // leak fails as itself instead of as a downstream timing flake three
            // sessions later (see Views.MainPage.LiveSyncAgentPollers).
            ["diagnostics"] = new Dictionary<string, object?>
            {
                ["live_sync_agent_pollers"] = Views.MainPage.LiveSyncAgentPollers,
                // Feed pages still attached to the session's feed manager: at most 1
                // while the Feed page is on screen, 0 anywhere else. A left Feed page
                // that stays attached re-renders on every manager tick and blanks the
                // app-wide error mirror under the page on screen (see
                // Views.FeedPage.LiveFeedPages; apps/windows.md § Client lifetime).
                ["live_feed_pages"] = Views.FeedPage.LiveFeedPages,
                // Registered ConversationsManager observer count: the manager-wide total -- the once-per-manager app-lifetime
                // observers plus exactly one page-scoped observer, which must never
                // grow however many times the Conversations tab is re-navigated. Always
                // via ConversationsManagerHost.Instance -- the production per-session
                // ConversationsSession is always built OVER this same instance
                // (ConversationsManagerHost's own class doc), never a fresh one, so
                // this reads the same manager a live page renders off regardless of
                // which wrapper obtained it.
                ["conversations_observer_count"] =
                    FaunaApp.Conversations.ConversationsManagerHost.Instance.ObserverCount(),
            },
            // `_currentErrorMessage` is the app-wide mirror of whatever error is on
            // screen: every page writes it on the UI thread as it opens/closes its own
            // error bar, and SerializeState reads it from the thread pool (hence
            // volatile). A page that opens its bar WITHOUT publishing here is invisible
            // to `ActionLayer.error_text()`/`has_error()`, which consult the state
            // protocol first and never fall through to the element once the `messages`
            // key exists — a surfaced failure then reads identically to a swallowed
            // one. Linux resolves its own `messages.error` the same way
            // (`apps/fauna-linux/src/lib.rs`, the `error_text` block).
            //
            // `_agentCommandFailure` OUTRANKS it, on every page (convention 11, and
            // the same precedence linux gives `agent_command_failure` and tui gives
            // `refused_agent_command`): a refused or failed agent command means the
            // app never did what the driver asked, so whatever error happens to be on
            // screen is at best unrelated and at worst the downstream symptom the
            // refusal explains. `??` rather than a nested read so an empty slot costs
            // nothing — this whole assembly is the ack path (convention 11's second
            // corollary): field reads only, never a round trip.
            //
            // `_injectedErrorMessage` sits between the two: an e2e-injected error is
            // what the driver asked the screen to show, so it outranks the page's own
            // mirror (tui's `error_line_text` gives `injected_error` the same win), and
            // it has a slot of its own because the mirror is rewritten by every page
            // re-render — see the field.
            ["messages"] = new Dictionary<string, object?>
            {
                ["error"] = _agentCommandFailure ?? _injectedErrorMessage ?? _currentErrorMessage,
                ["warning"] = _currentWarningMessage,
                ["info"] = _currentInfoMessage,
            },
            // Top-level, a sibling of nav/session/messages — onboarding.md § E2E
            // bridge contract's documented shape (linux's reference state_json
            // puts it here too); null unless the last call_machine_method was a
            // reader (provisioning_snapshot, provider_base_url, …).
            ["machine_method_result"] = _machineMethodResult,
            // The `serve_enable_folder` test-agent command's outcome — top-level,
            // the same depth linux's `webdav_serve_reply` sits at
            // (`apps/fauna-linux/src/lib.rs`), so
            // `helpers/webdav_roundtrip.py::serve_enable_folder`'s poll loop reads
            // one shape regardless of app. Null between invocations and while one
            // is in flight (cleared by the case itself before returning).
            ["webdav_serve_reply"] = _webdavServeReply,
            // The `enable_caldav_mailbox` test-agent command's outcome — top-level,
            // the same depth linux's `caldav_mailbox_reply` sits at
            // (`apps/fauna-linux/src/lib.rs`), so
            // `helpers/mail_dedicated_nest.py::mint_caldav_mailbox`'s poll loop reads
            // one shape regardless of app. Null between invocations and while one
            // is in flight (cleared by the case itself before returning).
            ["caldav_mailbox_reply"] = _caldavMailboxReply,
            // Convention 17's "a region Block never renders silent" counts
            // (`helpers/frame_invariants.py::_check_region_block_never_silent`) —
            // top-level, the key tui/linux/web/apple publish: `blocked` from each
            // on-screen surface's verdict, `placeholders` from the block placeholders
            // actually painted (RegionPlaneHost.BlockRenderState). Two set counts —
            // field reads only, as this ack path requires.
            ["region_block_render"] = Core.Services.RegionPlaneHost.BlockRenderState(),
            // The nest source's twin (`_check_nest_block_never_silent`, nest-rules.md
            // § Element IDs): the on-screen items a nest rule blocks against the nest
            // block placeholders painted (NestRulesHost.BlockRenderState) — two set
            // counts, field reads only.
            ["nest_block_render"] = Core.Services.NestRulesHost.BlockRenderState(),
            // The shell's one-line changed notice while it is up, else null
            // (`tests/e2e-unified/tests/test_nest_rules.py`; the notice carries no id).
            ["nest_rules_changed_notice"] = Core.Services.NestRulesHost.ChangedNoticeText,
            // The witness that a guardian's save reached this ward's device: how many
            // rules the guardian's document holds here, read through the shared
            // editor's own view (never counted in C#); null for an unsupervised viewer
            // or a policy with no document. tui's, android's and apple's twin.
            ["ward_rule_document_rules"] = WardRuleDocumentRules(),
            ["data"] = new Dictionary<string, object?>
            {
                // null = not implemented (test skips), populated = real data
                ["feed"] = feedPosts is not null
                    ? new Dictionary<string, object?> { ["posts"] = feedPosts }
                    : (object?)null,
                ["notifications"] = notifications,
                ["conversations"] = (object?)conversations,
                ["conversation_threads"] = conversationThreads,
                // The list's active order — the rows alone cannot name it (shared
                // conversation_sort_json, also published by tui/linux/macos/ios).
                ["conversation_sort"] = conversationSort,
                ["contacts"] = (object?)contacts,
                // The mounted profile page's viewed actor + actor-home slot
                // (`profile.actor_id`, `profile.home_nest_url`); null = no profile page.
                ["profile"] = ProfileStateNow is { } ps
                    ? new Dictionary<string, object?>
                    {
                        ["actor_id"] = ps.ActorId,
                        ["home_nest_url"] = ps.Home,
                    }
                    : (object?)null,
                ["events"] = (object?)events,
                ["knocks"] = (object?)knocks,
                ["sync"] = (object?)sync,
                // Readiness of the real wire-backed FaunaMls manager, polled by
                // actions/conversations.py::enable_real_faunamls. Twin of the linux
                // key (main.rs, fed by conv_backend::is_e2e_real_active).
                ["conv_real_backend_active"] =
                    FaunaApp.Conversations.ConversationsManagerHost.IsRealManagerRegistered,
                // The post-succession group sweep's own account of what it managed —
                // `SweepStatus::state_json` from the shared crate, republished
                // VERBATIM rather than re-encoded here (the shape is the cross-app
                // contract, so it lives on the shared status; e2e convention 11).
                //
                // ⚠ ABSENT stays null rather than an empty object, for the reason the
                // shared `state_json_or_null` documents: a journey must be able to
                // tell "no succession ran" from "one ran and swept nothing". It also
                // rides across the account switch on purpose — SuccessionHandoff is
                // exactly the state declared to outlive that teardown.
                ["succession_sweep"] = SuccessionSweepForSerialization(),
                // The member-side succession report — the receive-side twin of
                // `succession_sweep` above, republished VERBATIM (decoded, never
                // re-encoded) from `FfiNestClient::succession_witness_state_json`
                // (`succession-propagation.md` § Propagation → *MLS groups*, the
                // ✅ witness bullet: windows' whole state-contract obligation for
                // it). Null until a conversations session exists, for the same
                // "no session" vs. "a session that has seen nothing" reason
                // `succession_sweep` documents.
                ["succession_witness"] = SuccessionWitnessForSerialization(),
            },
        };
        // ABSENT until the --autostart activation gate at the foot of OnLaunched
        // has run (see _launchWindowShown), and added wholesale rather than as a
        // null-valued key so "the gate has not run yet" and "the gate ran and
        // chose hidden" can never be confused. A test waits for the KEY, then
        // asserts the value.
        if (_launchWindowShown is bool windowShown)
        {
            state["launch"] = new Dictionary<string, object?>
            {
                ["autostart"] = _launchAutostart is bool auto && auto,
                ["window_shown"] = windowShown,
                // Null when no window was shown (the tray-resident arm).
                ["window_activated"] = _launchWindowActivated is bool activated ? activated : null,
            };
        }
        // The screen-capture posture (security.md § On-screen secret exposure; the
        // running-app witness in its § Implementation status today): TOP-level, the
        // shape apple's ScreenCapturePosture publishes, read by
        // helpers/capture_posture.py.
        state["capture_posture"] = CapturePostureForSerialization();
        return state;
    }

    /// <summary>The main window's HWND, resolved on the UI thread where the window is
    /// built: <c>WinRT.Interop.WindowNative.GetWindowHandle</c> calls into a thread-affine
    /// WinRT object, and <see cref="SerializeState"/> runs on the agent's thread pool.</summary>
    private static volatile IntPtr _mainWindowHandle;

    /// <summary><c>capture_posture</c>: <c>mechanism</c> = <c>window-exclusion</c>
    /// (<c>WDA_EXCLUDEFROMCAPTURE</c>), <c>holds</c> = every live hold the shared guard
    /// keeps across windows, and <c>window_excluded</c> = the main window's REAL display
    /// affinity read back with <c>GetWindowDisplayAffinity</c> — left out, never a
    /// stand-in <c>false</c>, while there is no window or the platform will not answer.
    /// A user32 read, so the state provider still does no blocking I/O
    /// (convention 11).</summary>
    private static Dictionary<string, object?> CapturePostureForSerialization()
    {
        var guard = Core.Services.ScreenCaptureGuard.Shared;
        var posture = new Dictionary<string, object?>
        {
            ["mechanism"] = "window-exclusion",
            ["holds"] = guard.TotalHolds,
        };
        if (guard.WindowExcluded(_mainWindowHandle) is bool excluded)
        {
            posture["window_excluded"] = excluded;
        }
        return posture;
    }

    /// <summary>
    /// The sweep report as the state serializer publishes it: the DECODED object, or
    /// null when no succession ran on this app run.
    ///
    /// <para>Decoded rather than passed through as a JSON string, because
    /// <c>data.succession_sweep</c> is read as an object by the cross-app journeys.
    /// A blob this build cannot parse degrades to null rather than to a string the
    /// reader would then have to sniff.</para>
    /// </summary>
    private static object? SuccessionSweepForSerialization()
    {
        var json = Core.Services.SuccessionHandoff.SweepStateJson;
        if (string.IsNullOrEmpty(json)) return null;
        try
        {
            return System.Text.Json.JsonSerializer.Deserialize<object>(json);
        }
        catch (System.Text.Json.JsonException)
        {
            return null;
        }
    }

    /// <summary>
    /// The member-side succession report as the state serializer publishes it:
    /// the DECODED object, or null when no conversations session exists yet.
    ///
    /// <para>Decoded rather than passed through as a JSON string, because
    /// <c>data.succession_witness</c> is read as an object by the cross-app
    /// journeys — same reasoning as <see cref="SuccessionSweepForSerialization"/>,
    /// whose own doc covers the double-encode trap this also avoids. Reads
    /// <see cref="_rpcClient"/>, unlike that sibling: the witness is live session
    /// state (<c>FfiNestClient::succession_witness_state_json</c>), not a value
    /// carried across the account switch.</para>
    /// </summary>
    private object? SuccessionWitnessForSerialization()
    {
        var json = _rpcClient?.SuccessionWitnessStateJson();
        if (string.IsNullOrEmpty(json)) return null;
        try
        {
            return System.Text.Json.JsonSerializer.Deserialize<object>(json);
        }
        catch (System.Text.Json.JsonException)
        {
            return null;
        }
    }

    /// <summary>
    /// The <c>launch_token</c> state value DECODED (never the raw JSON string — the
    /// double-encode trap <see cref="MlsFoldedCommitsForSerialization"/> names):
    /// <c>{"expires_in_secs", "own_session_ids"}</c> from the live client's held
    /// bearer, built by shared Rust (<c>FfiNestClient.LaunchTokenJsonForTest</c>, a
    /// non-minting, non-waiting read). <c>null</c> — the key's documented "cannot
    /// answer yet" — with no connected client or while a mint is in flight.
    /// </summary>
    private object? LaunchTokenForSerialization()
    {
        var json = _rpcClient?.LaunchTokenJsonForTest();
        return json is null ? null : System.Text.Json.JsonSerializer.Deserialize<object>(json);
    }

    /// <summary>The <c>launch_refresh_token</c> agent command's mechanism on the live
    /// client (<c>NestRpcClient.RefreshHeldBearerForTestAsync</c>): drop the held
    /// bearer and re-mint over the silent challenge, awaited. <c>false</c> with no
    /// connected client.</summary>
    internal static Task<bool> RefreshHeldBearerForTestAsync() =>
        (Current as App)?._rpcClient?.RefreshHeldBearerForTestAsync() ?? Task.FromResult(false);

    /// <summary>
    /// Convention 14's <c>mls_folded_commits</c> observable as the state
    /// serializer publishes it: the DECODED <c>{channel_hex: count}</c> object,
    /// never the raw JSON string (which would double-encode at the wire
    /// boundary — <see cref="SuccessionSweepForSerialization"/> hits the same
    /// trap). No session yet (<see cref="_e2eRealConvSession"/> is null)
    /// returns <c>{}</c> rather than omitting the key — the shared-Rust
    /// contract's own pre-session arm
    /// (<c>fauna_conversations::state_json::mls_folded_commits_json_for_session</c>):
    /// a legitimate zero for every channel, not "windows hasn't built this leg".
    /// </summary>
    private object MlsFoldedCommitsForSerialization()
    {
        if (_e2eRealConvSession is null) return new Dictionary<string, object?>();
        var json = _e2eRealConvSession.MlsFoldedCommitsJson();
        return System.Text.Json.JsonSerializer.Deserialize<object>(json)
            ?? new Dictionary<string, object?>();
    }

    /// <summary>
    /// The <c>mls_bound_channels</c> state value: the DECODED sorted array of
    /// the channel hex ids the session's MLS backend has bound a thread to,
    /// never the raw JSON string (the double-encode trap
    /// <see cref="MlsFoldedCommitsForSerialization"/> names). No session yet
    /// returns <c>[]</c> rather than omitting the key — the shared-Rust
    /// contract's own pre-session arm
    /// (<c>fauna_conversations::state_json::mls_bound_channels_json_for_session</c>):
    /// windows HAS this leg, so <c>null</c>/absent stays reserved for an app
    /// that does not.
    /// </summary>
    private object MlsBoundChannelsForSerialization()
    {
        if (_e2eRealConvSession is null) return new List<object?>();
        var json = _e2eRealConvSession.MlsBoundChannelsJson();
        return System.Text.Json.JsonSerializer.Deserialize<object>(json)
            ?? new List<object?>();
    }

    /// <summary>
    /// Convention 14's <c>conv_receive_cycles</c> observable as the state
    /// serializer publishes it: the DECODED <c>{"started": N, "completed": M,
    /// "exit": null | "closed" | "retired" | "panicked"}</c> object, never the raw
    /// JSON string (same double-encode trap as <see
    /// cref="MlsFoldedCommitsForSerialization"/>). No session yet, or a decode
    /// failure, returns <c>{"started": 0, "completed": 0, "exit": null}</c> — the
    /// shared-Rust contract's own pre-session arm
    /// (<c>fauna_conversations::state_json::conv_receive_cycles_json</c>'s
    /// <c>None</c> branch), a fixed-key counter pair rather than
    /// <see cref="MlsFoldedCommitsForSerialization"/>'s open per-channel map.
    /// </summary>
    private object ConvReceiveCyclesForSerialization()
    {
        if (_e2eRealConvSession is null)
        {
            return new Dictionary<string, object?> { ["started"] = 0L, ["completed"] = 0L, ["exit"] = null };
        }
        var json = _e2eRealConvSession.ConvReceiveCyclesJson();
        return System.Text.Json.JsonSerializer.Deserialize<object>(json)
            ?? new Dictionary<string, object?> { ["started"] = 0L, ["completed"] = 0L, ["exit"] = null };
    }

    /// <summary>
    /// Convention 14's <c>feed_reloads</c> observable as the state serializer
    /// publishes it: the DECODED <c>{"started": N, "completed": M,
    /// "committed_gen": G}</c> object, never the raw JSON string (same
    /// double-encode trap as <see cref="ConvReceiveCyclesForSerialization"/>).
    /// Reads <c>Core.FeedManagerHost.Current</c> — the session's ONE live
    /// <c>FfiFeedManager</c>, the same one the Feed page observes — and
    /// no manager yet returns the shared contract's own pre-manager arm
    /// (<c>fauna_feed::feed_reloads_json(None)</c>'s zero triple) rather than
    /// omitting the key: windows HAS this leg.
    /// </summary>
    private static object FeedReloadsForSerialization()
    {
        var zero = new Dictionary<string, object?>
        {
            ["started"] = 0L,
            ["completed"] = 0L,
            ["committed_gen"] = 0L,
        };
        var json = FaunaApp.Core.FeedManagerHost.Current?.FeedReloadsJson();
        if (json is null) return zero;
        return System.Text.Json.JsonSerializer.Deserialize<object>(json) ?? zero;
    }

    /// <summary>
    /// The <c>devices_refreshes</c> barrier as the state serializer publishes it:
    /// the DECODED <c>{"started", "completed", "committed_gen"}</c> object, read
    /// once off the session's one <c>DevicesMachine</c>
    /// (<c>Core.DevicesMachineHost</c>, <c>refreshes_json</c>'s string form), and
    /// the zero triple before either Devices or Folders page has built it.
    /// </summary>
    private static object DevicesRefreshesForSerialization()
    {
        var zero = new Dictionary<string, object?>
        {
            ["started"] = 0L,
            ["completed"] = 0L,
            ["committed_gen"] = 0L,
        };
        var json = FaunaApp.Core.DevicesMachineHost.Current?.RefreshesJson();
        if (json is null) return zero;
        return System.Text.Json.JsonSerializer.Deserialize<object>(json) ?? zero;
    }

    /// <summary>
    /// The <c>serving_enablement</c> observable as the state serializer publishes
    /// it: the DECODED <c>{"started": N, "completed": M, "runs": [{"actor_id",
    /// "decided", "completed"}, …]}</c> object, never the raw JSON string (same
    /// double-encode trap as <see cref="ConvReceiveCyclesForSerialization"/>). The
    /// value is the shared derivation's own text
    /// (<c>serving_enablement_json()</c>) re-parsed, so the shape is never
    /// re-derived per app; the recorder is process-global, so no live client is
    /// needed and the pre-handoff shape is the shared empty one. The fallback below
    /// only guards a literal JSON <c>null</c>.
    /// </summary>
    private static object ServingEnablementForSerialization()
    {
        var json = uniffi.fauna_ffi.FaunaFfiMethods.ServingEnablementJson();
        return System.Text.Json.JsonSerializer.Deserialize<object>(json)
            ?? new Dictionary<string, object?>
            {
                ["started"] = 0L,
                ["completed"] = 0L,
                ["runs"] = Array.Empty<object>(),
            };
    }

#if P2P_SHARE
    /// <summary>The <c>share_serve_tally</c> observable: <c>{manifests: {path: n},
    /// chunks: {path: n}, parked: n, held: b}</c>, the FFI mirror of the shared
    /// recorder's snapshot. Windows HAS this leg, so the key is always published.</summary>
    private static object ShareServeTallyForSerialization()
    {
        var tally = uniffi.fauna_ffi.FaunaFfiMethods.ShareServeTally();
        return new Dictionary<string, object?>
        {
            ["manifests"] = tally.manifests,
            ["chunks"] = tally.chunks,
            ["parked"] = tally.parked,
            ["held"] = tally.held,
        };
    }
#endif

    /// <summary>
    /// The <c>message_banners</c> observable as the state serializer publishes it:
    /// the DECODED <c>{"started": N, "completed": M, "fired": [{"thread_id",
    /// "label"}, …]}</c> object, never the raw JSON string (same double-encode trap
    /// as <see cref="ConvReceiveCyclesForSerialization"/>). The recorders are
    /// process-global, so there is no pre-session arm and no fallback shape: a
    /// fabricated zero object would answer "the leg exists and nothing fired" when
    /// the truth is unknown — exactly the vacuous negative the key's contract forbids
    /// (<c>fauna_e2e_agent::MESSAGE_BANNERS_KEY</c>). Windows HAS this leg, so the key
    /// is always published.
    /// </summary>
    private static object? MessageBannersForSerialization() =>
        System.Text.Json.JsonSerializer.Deserialize<object>(
            uniffi.fauna_conversations.FaunaConversationsMethods.MessageBannersJsonText());

    /// <summary>
    /// The <c>ward_rule_document_rules</c> observable (tui's, android's and apple's
    /// <c>ContentPolicyTestState.wardRuleDocumentRules</c>; <c>test_nest_rules.py</c>'s
    /// guardian journey): how many rules the guardian's document holds on this ward's
    /// device, read through the shared editor's own view
    /// (<c>FfiRuleEditor.ForGuardian(policy).View().rows</c>) — never counted in C#.
    /// <c>null</c> for an unsupervised viewer or a policy with no document. An
    /// in-process FFI read over the held snapshot, no I/O (convention 11).
    /// </summary>
    private static int? WardRuleDocumentRules()
    {
        var policy = Core.Services.ContentPolicyCache.Current.ContentPolicy;
        if (policy?.document is null) return null;
        try
        {
            using var editor = uniffi.fauna_ffi.FfiRuleEditor.ForGuardian(policy);
            return editor.View().rows.Length;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"ward rule document unreadable for the state field: {ex.GetType().Name}: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// The cross-app <c>connection</c> observable
    /// (<c>fauna_e2e_agent::CONNECTION_KEY</c>) as the state serializer
    /// publishes it: <c>{"state": word, "online": bool}</c>, built the same way
    /// every non-Rust app builds it (android's <c>TestAgent.kt</c>, apple's
    /// <c>AppStateObservables.swift</c>) — the word from this app's own gate,
    /// the boolean from the shared <c>connection_is_online</c> UniFFI door,
    /// never a local <c>== "connected"</c> comparison.
    /// </summary>
    private static object ConnectionForSerialization()
    {
        var word = OfflineGate.Shared.ConnectionState;
        return new Dictionary<string, object?>
        {
            ["state"] = word,
            ["online"] = uniffi.fauna_ffi.FaunaFfiMethods.ConnectionIsOnline(word),
        };
    }

    /// <summary>
    /// The cross-app <c>clock</c> observable (<c>fauna_e2e_agent::CLOCK_KEY</c>)
    /// as the state serializer publishes it: <c>{"offset_secs": i64,
    /// "now_secs": i64}</c>. Both values are read from shared Rust
    /// (<c>fauna_launch_machine::launch_clock</c>) through the
    /// <c>windows-ffi-test</c> flavor's two <c>*ForTest</c> getters — never
    /// computed here — so windows reports the exact clock the launch machine's
    /// silent challenge ran under, on the same shape every other app publishes.
    /// Inside the <c>DEBUG || FAUNA_E2E_AGENT</c> region like every state key
    /// (convention 15): the getters do not exist in the production flavor.
    /// </summary>
    private static object ClockForSerialization() =>
        new Dictionary<string, object?>
        {
            ["offset_secs"] = uniffi.fauna_ffi.FaunaFfiMethods.LaunchClockOffsetSecsForTest(),
            ["now_secs"] = uniffi.fauna_ffi.FaunaFfiMethods.LaunchClockNowSecsForTest(),
        };

    /// <summary>
    /// Convention 14's <c>conv_receive_now</c> poke — runs one receive cycle NOW
    /// on the live e2e session, fire-and-forget (the ack is deliberately NOT the
    /// barrier; <c>conv_receive_cycles</c> above is). Called directly from
    /// <c>TestAgent.ProcessCommand</c>'s <c>conv_receive_now</c> case, the same
    /// static-call shape as <see cref="FreezeBarrierAckProbe"/> (WinUI has one
    /// <c>App</c> instance; <c>Current</c> reaches it). No session yet is a
    /// legitimate quiet no-op, per the shared-Rust contract
    /// (<c>ConversationsSession::poke_receive_cycle</c>'s UniFFI twin
    /// <c>conv_receive_now</c>) — never an error.
    /// </summary>
    internal static void ConvReceiveNowForTest() =>
        (Current as App)?._e2eRealConvSession?.ConvReceiveNow();

    /// <summary>
    /// Convention 14's <c>account_pump_cycles</c> observable as the state
    /// serializer publishes it: the DECODED <c>{"started": N, "completed": M,
    /// "runtime": bool, "holder": bool}</c> object, never the raw JSON string
    /// (same double-encode trap as <see cref="ConvReceiveCyclesForSerialization"/>).
    /// No connected client yet returns the shared-Rust contract's own
    /// pre-session shape (<c>fauna_client_account_runtime::account_pump_cycles_json</c>'s
    /// <c>None</c> branch) rather than omitting the key — windows HAS this leg.
    /// </summary>
    private object AccountPumpCyclesForSerialization()
    {
        var fallback = new Dictionary<string, object?>
        {
            ["started"] = 0L, ["completed"] = 0L, ["runtime"] = false, ["holder"] = false,
        };
        var json = _rpcClient?.AccountPumpCyclesJson();
        if (string.IsNullOrEmpty(json)) return fallback;
        return System.Text.Json.JsonSerializer.Deserialize<object>(json) ?? fallback;
    }

    /// <summary>
    /// The <c>account_pump_now</c> poke — one full account-pump pass now on the
    /// live e2e client, fire-and-forget (the ack is deliberately NOT the
    /// barrier; <c>account_pump_cycles</c> above is). Called directly from
    /// <c>TestAgent.ProcessCommand</c>'s <c>account_pump_now</c> case, the same
    /// static-call shape as <see cref="ConvReceiveNowForTest"/>. No connected
    /// client or no assembled runtime yet (pre-auth) is a legitimate quiet
    /// no-op — <c>AccountPumpNowAsync</c> reports it, never throws.
    /// </summary>
    internal static void AccountPumpNowForTest() =>
        _ = (Current as App)?._rpcClient?.AccountPumpNowAsync();

    /// <summary>The <c>custody_ceremony_ingest</c> state value, decoded from the
    /// FFI's shared-builder JSON; <c>null</c> before a client exists.</summary>
    private object? CustodyCeremonyIngestForSerialization()
    {
        var json = _rpcClient?.CustodyCeremonyIngestJson();
        return string.IsNullOrEmpty(json) ? null : System.Text.Json.JsonSerializer.Deserialize<object>(json);
    }

    /// <summary>
    /// The <c>custody_owner_resend_now</c> poke on the live e2e client —
    /// fire-and-forget, the HOST's <c>custody_ceremony_ingest</c> being the
    /// barrier (<c>fauna_e2e_agent::CUSTODY_OWNER_RESEND_NOW</c>). Called from
    /// <c>TestAgent.ProcessCommand</c>'s <c>custody_owner_resend_now</c> case, the
    /// same static-call shape as <see cref="AccountPumpNowForTest"/>. No client,
    /// session or account store yet is a quiet no-op the callee reports.
    /// </summary>
    internal static void CustodyOwnerResendNowForTest() =>
        _ = (Current as App)?._rpcClient?.CustodyOwnerResendNowAsync();

    /// <summary>
    /// The <c>device_set_state</c> e2e reader on the live client — the raw JSON
    /// from <c>NestRpcClient.DeviceSetStateJsonAsync</c>, or <c>null</c> when no
    /// client is connected (pre-auth), which <c>TestAgent</c> reports as
    /// <c>{"found":false}</c>. Reached through this accessor because
    /// <c>_rpcClient</c> is private; the reader deliberately stays off
    /// <see cref="Core.Services.INestRpcClient"/>, which would drag every mock
    /// implementation along for a Debug-only seam. Called from
    /// <c>TestAgent.ProcessCommand</c>'s <c>device_set_state</c> case.
    /// </summary>
    internal static Task<string?> DeviceSetStateForTest(string deviceIdHex) =>
        (Current as App)?._rpcClient?.DeviceSetStateJsonAsync(deviceIdHex)
        ?? Task.FromResult<string?>(null);

    /// <summary>
    /// The <c>read_marker_state</c> e2e reader on the live client — the raw JSON
    /// from <c>NestRpcClient.ReadMarkerStateJsonAsync</c>, or <c>null</c> when no
    /// client is connected (pre-auth), which <c>TestAgent</c> reports as
    /// <c>{"found":false}</c>. The <see cref="DeviceSetStateForTest"/> shape.
    /// </summary>
    internal static Task<string?> ReadMarkerStateForTest(string channelIdHex) =>
        (Current as App)?._rpcClient?.ReadMarkerStateJsonAsync(channelIdHex)
        ?? Task.FromResult<string?>(null);

    /// <summary>
    /// The <c>escrow_wrap_coordinates</c> e2e reader on the live client — the raw
    /// JSON from <c>NestRpcClient.EscrowWrapCoordinatesJsonAsync</c>, or
    /// <c>null</c> when no client is connected (pre-auth), which <c>TestAgent</c>
    /// reports as <c>{"found":false}</c>. The <see cref="DeviceSetStateForTest"/>
    /// shape.
    /// </summary>
    internal static Task<string?> EscrowWrapCoordinatesForTest() =>
        (Current as App)?._rpcClient?.EscrowWrapCoordinatesJsonAsync()
        ?? Task.FromResult<string?>(null);

    /// <summary>
    /// The <c>reconnect_backoff</c> seam on the live client
    /// (<c>NestRpcClient.SetReconnectBackoffForTest</c>). <c>false</c> when there
    /// is no client to pace, which <c>TestAgent</c> refuses loudly (convention 11).
    /// </summary>
    internal static bool SetReconnectBackoffForTest(string payloadJson) =>
        (Current as App)?._rpcClient?.SetReconnectBackoffForTest(payloadJson) ?? false;

    /// <summary>
    /// A shared-Rust JSON observable decoded for the state serializer — never the
    /// raw string (the double-encode trap <see cref="ConvReceiveCyclesForSerialization"/>
    /// names). Used for <c>connection_reports</c>, <c>painted_errors</c> and
    /// <c>knock_notices</c>, whose logs are process-wide and start empty, so there is
    /// no pre-session arm.
    /// </summary>
    private static object? DecodeObservable(string json) =>
        System.Text.Json.JsonSerializer.Deserialize<object>(json);

    /// <summary>
    /// Arrange a WebDAV-served, content-keyed folder for the currently
    /// logged-in actor — the windows twin of linux's
    /// <c>serve_enable_folder_for_test</c>/<c>serve_enable_folder_flow</c>
    /// (<c>apps/fauna-linux/src/client.rs</c>), wired over the SAME production
    /// slice-6b composition the Folders page's serve toggle drives
    /// (<see cref="Core.Services.INestRpcClient.FoldersServeSetAsync"/> →
    /// the shared <c>FoldersAuthor::serve_set</c>), never the raw FFI unseal
    /// door (<c>webdav-server.md</c> § Implementation status today, slice 6b).
    /// When <paramref name="create"/>, mints an empty folder first —
    /// the test's own precondition (an empty served set is all a WebDAV
    /// PUT/GET round-trip needs; mirrors linux's <c>FoldersClient::create</c>).
    /// The new set is unshared, so <c>mlsGroupIdHex</c> is <c>null</c>
    /// (owner-only). Called from <c>TestAgent</c>'s <c>serve_enable_folder</c>
    /// case, which owns writing the outcome to <see cref="WebdavServeReply"/>.
    /// </summary>
    internal static async Task<(bool Ok, uint ServedSets, string? Error)> WebdavServeEnableFolderForTest(
        string folder, bool create)
    {
        var app = Current as App;
        var rpc = app?._rpcClient;
        var convSession = app?._e2eRealConvSession;
        if (rpc is null || convSession is null)
        {
            return (false, 0, "no live nest/conversations session");
        }
        try
        {
            if (create)
            {
                await rpc.FoldersCreateAsync(folder).ConfigureAwait(false);
            }
            var served = await rpc.FoldersServeSetAsync(convSession, folder, null, true, app?._e2eRealConvDeviceIdHex).ConfigureAwait(false);
            // Serving moves the set's key; the agent re-keys on the custody write's
            // account-state change itself (on-demand-files-shared-sets.md § Shared sets on a capability
            // host → One mechanism).
            return (true, served, null);
        }
        catch (Exception ex)
        {
            return (false, 0, $"{ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Mint the currently logged-in actor's shared CalDAV MSEK — the windows
    /// twin of linux's <c>enable_caldav_mailbox_for_test</c>
    /// (<c>apps/fauna-linux/src/client.rs</c>), wired over the SAME production
    /// door the mail-settings CalDAV toggle drives
    /// (<see cref="Core.Services.INestRpcClient.BuildMailSettingsMachineAsync"/>
    /// → the shared <c>MailSettingsMachine::enable_caldav_mailbox*</c>), never
    /// the raw FFI door. A caller-provided <paramref name="password"/> mints a
    /// <c>default</c> credential the test knows (so a stock CalDAV client can
    /// AUTH as this actor); <c>null</c> generates one. Called from
    /// <c>TestAgent</c>'s <c>enable_caldav_mailbox</c> case, which owns writing
    /// the outcome to <see cref="CaldavMailboxReply"/>.
    /// </summary>
    internal static async Task<(bool Ok, string? Error)> EnableCaldavMailboxForTest(string? password)
    {
        var app = Current as App;
        var rpc = app?._rpcClient;
        if (rpc is null)
        {
            return (false, "no live nest session");
        }
        try
        {
            var machine = await rpc.BuildMailSettingsMachineAsync().ConfigureAwait(false);
            if (!string.IsNullOrEmpty(password))
            {
                await machine.EnableDavMailboxWithPassword("Default", password).ConfigureAwait(false);
            }
            else
            {
                await machine.EnableDavMailboxWithGeneratedPassword("Default").ConfigureAwait(false);
            }
            return (true, null);
        }
        catch (Exception ex)
        {
            return (false, $"{ex.GetType().Name}: {ex.Message}");
        }
    }
#endif

    /// <summary>
    /// Drop every piece of in-memory actor-scoped state this app holds. <b>The</b>
    /// canonical drop — the one call each of the five teardown sites makes, with no
    /// list of its own: <c>SwitchAccountHandler</c>, <c>SignOutHandler</c>,
    /// <c>FactoryResetReonboardHandler</c>, <see cref="DisposeNestClients"/>, and the
    /// test agent's <c>reset</c>/<c>logout</c> arms.
    ///
    /// <para><c>account-scoping.md</c> § Isolation-contract gap ledger, the
    /// <c>windows (in-memory)</c> row, owns this. Windows never exits the process on
    /// an actor change, and until 2026-08-24 the drop was hand-listed at those five
    /// sites — which had already drifted apart (the two content caches were on
    /// <b>none</b> of them; the conversations manager on the switch only; Guardian
    /// Notify and the e2e snapshot on all but factory-reset). That drift, not
    /// whichever piece happened to be missing, is the finding — the same one web,
    /// linux and apple each closed on the shape linux ruled: one explicit,
    /// statically greppable function rather than a registry that would relocate the
    /// hand-list and fail silently when a registration is forgotten.</para>
    ///
    /// <para>The Core-owned half (the caches, the memoized machines, and the
    /// cancellation seam that retires the background loops) lives in
    /// <see cref="Core.Services.ActorScope.DropActorScopedState"/> — read its header
    /// for the seam. Only the two surfaces that live in <i>this</i> assembly are
    /// listed here, because <c>FaunaApp.Core</c> cannot reach up into the WinUI
    /// project (<c>FaunaApp.csproj</c>'s one-way reference). That is apple's split
    /// verbatim: FaunaKit's <c>resetSharedState()</c> under each target's
    /// <c>dropActorScopedState()</c>.</para>
    ///
    /// <para>Site-specific work stays at its own site — disposing the nest clients,
    /// unprovisioning the sync agent, nilling the session fields
    /// (<c>_messageToastObserver</c>, <c>ConvDrafts</c>: rebuilt per authenticated
    /// dispatch, not process-lifetime state), rebuilding crypto, navigation,
    /// credential wipes, and the e2e-only <c>ClearForTest</c> arms. This drops
    /// <i>state</i>, exactly as linux's <c>reset_actor_scoped_state</c> does.</para>
    /// </summary>
    private static void DropActorScopedState(bool identityEnds = true)
    {
        Core.Services.ActorScope.DropActorScopedState(identityEnds);

        // The conversations manager carries this identity's MLS/SMTP rails,
        // observers, threads and drafts. Reusing it across an actor change would
        // have the incoming account encrypting and decrypting through the
        // SIGNED-OUT identity's rails — a silent cross-identity data bug. Replacing
        // this host is windows' seam: our MLS state is the per-session engine the
        // host carries (every app is on that rail since the standalone fauna-ffi
        // MLS plane was deleted 2026-07-22).
        FaunaApp.Conversations.ConversationsManagerHost.ResetForActorChange();

        // Same reasoning one surface over: the feed manager carries this
        // identity's timeline, feed selection, composer draft and engagement-cue
        // state, and rides this identity's authed transport. It is session-scoped
        // (Core.FeedManagerHost), so an actor change is exactly the boundary at
        // which it must be dropped rather than handed to the incoming account.
        Core.FeedManagerHost.ResetForActorChange();
        Core.PostBlobHostsHost.ResetForActorChange();

        // And the Devices/Folders pages' one session machine: it holds this
        // identity's folder, device and followed rows, and rides its transport.
        Core.DevicesMachineHost.ResetForActorChange();

        // The e2e state provider's reads. Not a production render path, but a stale
        // answer from the agent reads downstream as a product bug
        // (e2e-conventions.md point 11), so it is actor-scoped like the rest.
        Services.AppDataSnapshot.Clear();
    }

    /// <summary>
    /// Tear down the current WS-RPC / HTTP nest clients before they are replaced
    /// (re-auth to a different nest, reset, or logout). Both are
    /// <see cref="System.IAsyncDisposable"/>: <see cref="NestRpcClient.DisposeAsync"/>
    /// disconnects + disposes the inner <c>FfiNestClient</c> — which drops the watch
    /// senders so the reconnect / connection-state pump loops end and the native
    /// auto-reconnect stops — and <see cref="DirectNestClient.DisposeAsync"/> disposes
    /// its HttpClient. Without this, each re-auth overwrites
    /// <c>_rpcClient</c>/<c>_nestClient</c> and LEAKS the prior client: its
    /// connection-state pump (<see cref="MainViewModel"/>) keeps a reference to it, so
    /// it is never collected and its FfiNestClient keeps auto-reconnecting to the
    /// now-gone nest forever. The leak accumulates across set_state re-auths (e2e: one
    /// long-lived process re-pointed at N dedicated nests) and across production
    /// logout → login-to-another-nest. Fire-and-forget: teardown need not block the
    /// replacement, and DisposeAsync is ConfigureAwait(false) throughout, so it never
    /// marshals back to the UI thread. (Linux/web get this for free — Rust drops the
    /// old client on reassignment; only C# needs the explicit dispose.)
    /// </summary>
    /// <summary>
    /// CLOSE a retiring <c>ConversationsSession</c>, never merely drop it — windows'
    /// twin of android's <c>ConversationsManagerHost.stopConversationsSession()</c>
    /// (which has always called <c>session.close()</c>).
    ///
    /// <para><b>Why an explicit close, and why it is not optional.</b> The UniFFI
    /// object owns the <c>Arc&lt;FaunaMlsBackend&gt;</c> → <c>MlsEngine</c> →
    /// <c>SqliteStorage</c> that HOLDS the conversations-engine role lock over this
    /// account's <c>mls.db</c> (<c>libs/fauna-mls/src/storage.rs</c> — the lock is a
    /// live <c>File</c> handle, released only when the storage drops). The generated
    /// binding frees that Arc from a FINALIZER when it is merely dropped, so a nulled
    /// field leaves the lock held until a GC happens to run — and a freshly relaunched
    /// process has a young heap under no pressure, so in practice it never does. Two
    /// measured consequences, both permanent for the rest of the process's life:</para>
    /// <list type="number">
    ///   <item>the account-state erase (<c>FinishSignOut</c> → the shared sign-out
    ///   routine's <c>erase_all_account_scopes</c>, a <c>remove_dir_all</c> over the actor scope
    ///   dir) fails with Windows <c>ERROR_SHARING_VIOLATION</c> — <c>os error 32</c>
    ///   — because <c>mls.db</c> and its <c>.lock</c> are still open. It is swallowed
    ///   as a WARN, so a sign-out silently leaves the user's conversations readable on
    ///   disk (<c>principles.md</c> § The user always controls their data);</item>
    ///   <item>the next login's <c>SqliteStorage::open</c> finds the role lock held and
    ///   refuses with <c>StateServedElsewhere</c> — "served in another instance" — so
    ///   conversations stay dead for every later login in the process.</item>
    /// </list>
    /// <para>Unix never showed this: <c>unlink</c> succeeds against open handles there,
    /// so the same leak is invisible until Windows' sharing semantics surface it.</para>
    ///
    /// <para>Best-effort by construction: a teardown that threw would strand the
    /// caller mid-sign-out, and a failed close costs only the pre-fix behaviour.</para>
    /// </summary>
    /// <param name="pending">An in-flight build, if any. It must not outlive the
    /// teardown that retired it: whatever it produces owns an engine of its own, so it
    /// is closed on completion rather than left to the finalizer. Double-close is safe
    /// — the binding's <c>Destroy()</c> is <c>Interlocked</c>-guarded.</param>
    /// <param name="session">The already-built session, if any.</param>
    /// <summary>
    /// Hand this login's archive links to <see cref="Core.Services.ArchiveLinksHost"/>
    /// (archive-import.md § Linking and merging, ruling 8): read them off the
    /// <c>FfiNestClient</c> the conversations session was just built on (the factory
    /// built them and registered them as the session's sink), drive them once now, and
    /// let the host's one-second tick drive again on <c>LinkWorkDue()</c> and repaint the
    /// Contacts rows when <c>LinkCaptures()</c> moves. A session with no links (no sync
    /// device id) stops the host instead. Best-effort: a failed read costs phase two,
    /// never the login. The host is stopped on an actor change by
    /// <c>ActorScope.DropActorScopedState</c>.
    /// </summary>
    private static async Task StartArchiveLinksAsync(Core.Services.INestRpcClient rpc)
    {
        try
        {
            Core.Services.ArchiveLinksHost.Instance.Start(await rpc.ArchiveLinksAsync());
        }
        catch (Exception ex)
        {
            ShellLog.Warn("ArchiveLinks", $"start: {ex.Message}");
        }
    }

    /// <summary>
    /// Re-read the imported archives' message threads onto the conversations
    /// <c>Archive</c> rail (archive-import.md § What each category becomes, the
    /// message-threads row). The shared conversations factory registered the source
    /// over this login's recording device, so this only asks the session to read the
    /// archive folders again — at login and on every visit to the conversations page,
    /// so an import finished since login lists with no re-login (tui's
    /// <c>App::set_page</c>, linux's <c>conv_backend::refresh_archive_threads</c>,
    /// apple's <c>ConversationsVM.refreshArchiveThreads</c>). The read lands in the
    /// shared manager, which notifies the page's observer, so nothing here repaints.
    /// Best-effort: no session is a no-op, and a failed read keeps what the last one
    /// listed. No <c>ConfigureAwait(false)</c> (WinUI bound-state rule).
    /// </summary>
    internal static async Task RefreshArchiveThreadsAsync(ConversationsSession? session)
    {
        if (session is null) return;
        try
        {
            await uniffi.fauna_ffi.FaunaFfiMethods.RefreshArchiveThreads(session);
        }
        catch (Exception ex)
        {
            ShellLog.Info("ArchiveThreads", $"refresh: {ex.Message}");
        }
    }

    private static void RetireConvSession(
        Task<ConversationsSession?>? pending,
        ConversationsSession? session)
    {
        try { session?.Dispose(); } catch { /* teardown is best-effort — see above */ }
        if (pending is null) return;
        _ = pending.ContinueWith(
            t => { try { t.Result?.Dispose(); } catch { /* best-effort */ } },
            TaskContinuationOptions.OnlyOnRanToCompletion);
    }

    /// <param name="signOut">Whether the erase that follows takes the
    /// account-store slot (the writer key) with it — the e2e <c>reset</c>/
    /// <c>logout</c> command arms pass <c>true</c>, so their stop retires this
    /// machine's enrollment nest-side before <see cref="ClearCredentialNamespace"/>
    /// runs right behind this call, same as <see cref="ReleaseAccountScopedStoresBeforeErase"/>
    /// does for the real sign-out and "start over" paths (which reach the stop
    /// through that method instead, with a still-live <c>_rpcClient</c>, never
    /// through here). Every other caller — the account switch, the e2e
    /// <c>session</c> nest re-point — leaves the default <c>false</c>: they
    /// never erase the slot, so the switch-shaped stop is correct
    /// (<c>sync-agent-credentials.md</c> § Credential model → *The signed-out
    /// reconcile*).</param>
    private void DisposeNestClients(bool signOut = false, bool sameIdentity = false)
    {
        // The leave-flush, FIRST: its snapshots are taken here, before the session
        // retire and DropActorScopedState below drop the managers they read, and the
        // deferred disposal at the bottom waits for its PUTs before the client they
        // ride goes (FlushDraftRailsAsync's own contract). Every caller is a session's
        // transport going away — the account switch, the e2e reset/logout arms, a
        // nest re-point — and an edit still inside its debounce would otherwise fire
        // on the disposed client and be lost.
        var draftsFlushed = FlushDraftRailsAsync();

#if DEBUG || FAUNA_E2E_AGENT
        // The e2e conversations session is built ON the WS-RPC client being disposed
        // here, so it dies with it: drop both the in-flight build and the held Arc.
        // Otherwise a reset / logout / nest re-point would leave the NEXT navigation
        // (which awaits _e2eConvSessionTask) handing a page a session bound to a
        // disposed client — the same staleness class as the MainPage._clients.Rpc bug.
        // The next authenticated set_state re-builds it against the new client.
        // (Gated with the fields themselves: both exist only in a test-capable
        // build, so there is nothing to tear down in a production one.)
        RetireConvSession(_e2eConvSessionTask, _e2eRealConvSession);
        _e2eConvSessionTask = null;
        _e2eRealConvSession = null;
        _e2eFeedDraftsSyncTask = null;
#endif

        // The production session takes the same exit. Both paths build over
        // ConversationsManagerHost.Instance, so both leave an engine behind if the
        // wrapper is merely dropped — see RetireConvSession.
        RetireConvSession(null, _liveConvSession);
        _liveConvSession = null;

        // The sync-agent session's bearer source reads the clients being disposed
        // here — stop the loop with them. The next authenticated login (either
        // seam) starts a fresh one. NOT an unprovision: this path also covers a
        // nest re-point and the e2e re-login, where the agent must keep serving;
        // the three teardown paths that really end the session call
        // UnprovisionSyncAgentAsync themselves before reaching here.
        StopSyncAgentSession();

        // Every actor-scoped surface is bound to the clients dying three lines
        // below, or to the identity that owned them. The memoized Bluesky machine
        // is the case that made this site load-bearing: it is built from an
        // INestRpcClient, so the e2e `session` set_state re-login — same actor or
        // not — used to leave it reading through a disposed client. No exception
        // surfaces through the async chain; it just never finds anything again
        // (found via a live repro: a second OAuth consent request in the same
        // pytest process never rendered on the AT Protocol page after the fixture
        // re-logged in). The secret is not the invariant that matters here, the
        // CLIENT is. ONE call, no list — DropActorScopedState.
        //
        // The one thing that follows the identity rather than the client: a caller
        // rebuilding clients for the SAME actor (`sameIdentity`) keeps that actor's
        // standing critical alerts and the sweep loop re-checking them — see
        // ActorScope.DropActorScopedState's `identityEnds`.
        DropActorScopedState(identityEnds: !sameIdentity);

        var oldRpc = _rpcClient;
        var oldNest = _nestClient;
        _rpcClient = null;
        _nestClient = null;
        // The departing session's refresh context — a stale machine here would let
        // TriggerSilentSignInForTestAsync drive a dead LaunchMachine after logout.
        _liveTtlRefreshContext = null;
        // Tracked, not awaited: the teardown stays fire-and-forget for every
        // caller that only wants the client replaced, but every erase that runs
        // while it is still going has to wait for the store to close — the e2e
        // `reset`/`logout` arms' ClearCredentialNamespace on the very next line,
        // and a remove-account of the identity a switch just left
        // (AccountStateDir.RemoveAccountGestureAsync). Tracked in a set, never one
        // field: two switches in a row leave two stops running, and the erase
        // waits for both (Core.Services.AccountRuntimeStops).
        if (oldRpc is not null)
        {
            Core.Services.AccountRuntimeStops.Process.Track(
                StopAccountRuntimeThenDisposeAsync(oldRpc, signOut, draftsFlushed));
        }
        if (oldNest is not null) _ = oldNest.DisposeAsync();
    }

    /// <summary>
    /// Stop the W3 account-store runtime <paramref name="rpc"/> was hosting,
    /// THEN dispose it — the ordering sign-out / account-switch / factory-reset
    /// require (<see cref="NestRpcClient.StopAccountRuntimeAsync"/> /
    /// <see cref="NestRpcClient.StopAccountRuntimeForSignOutAsync"/> both await
    /// the pump's in-flight pass, so the departing account has stopped writing
    /// before the socket underneath it closes).
    ///
    /// <para>Wraps <see cref="DisposeNestClients"/>'s existing fire-and-forget
    /// dispose rather than making that method (and its many synchronous
    /// callers, including the e2e <c>reset</c>/<c>logout</c> command arms)
    /// async. Every one of its call sites is a departure this client was
    /// hosting the runtime for, and the stop is a documented no-op when no
    /// runtime was ever started — so running it unconditionally on a plain
    /// nest re-point costs nothing.</para>
    /// </summary>
    /// <param name="rpc">The departing client whose runtime to stop, then dispose.</param>
    /// <param name="signOut">See <see cref="DisposeNestClients"/>'s own parameter of the
    /// same name — selects the sign-out-shaped stop over the switch-shaped one.</param>
    /// <param name="draftsFlushed">The leave-flush <see cref="DisposeNestClients"/> started
    /// over this client — its PUTs ride it, so it is waited for (bounded) before anything
    /// here stops or disposes. <c>null</c> from the stale-client cleanup in
    /// <c>StartMainAppAsync</c>, which runs after its session's erase: nothing is left to
    /// flush there.</param>
    private static async Task StopAccountRuntimeThenDisposeAsync(
        NestRpcClient rpc, bool signOut = false, Task? draftsFlushed = null)
    {
        if (draftsFlushed is not null) await AwaitDraftFlushAsync(draftsFlushed).ConfigureAwait(false);
        if (signOut)
        {
            await rpc.StopAccountRuntimeForSignOutAsync().ConfigureAwait(false);
        }
        else
        {
            await rpc.StopAccountRuntimeAsync().ConfigureAwait(false);
        }
        await rpc.DisposeAsync().ConfigureAwait(false);
    }

    /// <summary>
    /// Release every holder of an account-scoped store, so nothing keeps one open
    /// when the erase below runs: the conversations SESSION (<c>mls.db</c>) and the
    /// account runtime (the unified account store, waited for). Apple's
    /// <c>StatusVM.signOut</c> has stopped the runtime since that seat landed
    /// (<c>await api?.stopAccountRuntime()</c> as its very first line); windows
    /// never did, and this is the drift that closed. Both sides now call the
    /// sign-out-shaped stop here, retiring this machine's enrollment nest-side
    /// before the erase takes the writer key with it
    /// (<c>sync-agent-credentials.md</c> § Credential model → *The signed-out
    /// reconcile*; <see cref="NestRpcClient.StopAccountRuntimeForSignOutAsync"/>).
    ///
    /// <para><b>The invariant is what to keep, not the list.</b> On windows an open
    /// file cannot be deleted at all, so the erase is only as complete as this
    /// method is: a new store holder added anywhere in the app owes a release
    /// HERE, or a sign-out silently leaves that store on disk. The two known
    /// holders are enumerated in the body, each with the path it locks.</para>
    ///
    /// <para><b>Two reasons for the runtime half, and the windows one is the loud.</b> The shared
    /// reason apple documents is resurrection: the runtime is a process global
    /// that keeps pumping until told otherwise
    /// (<c>account-data-plane.md</c> § The account store → <i>The client-side
    /// lifecycle</i>), so erasing the store out from under a live one lets the next
    /// pass re-create it — after the credential wipe has already taken its writer
    /// key, leaving the signed-out user's store on disk for the next sign-in to
    /// adopt (the lost-slot self-heal, since 2026-08-27; before it, that store
    /// refused every later sign-in). On windows it does
    /// not even get that far: <c>remove_dir_all</c> cannot delete an open file, so
    /// the erase fails with <c>ERROR_SHARING_VIOLATION</c> —
    /// <c>os error 32</c> — and leaves the signed-out user's whole account store
    /// on disk (reported since 2026-10-02 as a survivor on the
    /// <c>sign-out-residue</c> view, never a swallowed WARN)
    /// (<c>principles.md</c> § The user always controls their data). That is the
    /// SAME sharing-violation class <see cref="RetireConvSession"/> documents one
    /// seam over for <c>mls.db</c>; unix hides both, because <c>unlink</c> succeeds
    /// against open handles there.</para>
    ///
    /// <para><b>Why it waits on two things.</b> The sign-out path never disposes
    /// its clients (the session outlives the wipe), so its runtime is still hosted
    /// by a live <c>_rpcClient</c>; the e2e <c>reset</c>/<c>logout</c> arms call
    /// <see cref="DisposeNestClients"/> first, which nulls that field and starts the
    /// stop fire-and-forget — so by the time the erase runs there is nothing left to
    /// ask, only a stop in flight to wait for. Both kinds go through ONE barrier
    /// (<see cref="Core.Services.AccountRuntimeStops"/>), the same one the
    /// remove-account erase waits behind, and it holds while any stop is in flight —
    /// an earlier switch's included. Covering one and not the other would leave half
    /// the callers racing, which is how a single-site fix here re-breaks.</para>
    ///
    /// <para>Best-effort and bounded: a sign-out that hung on a wedged pump would be
    /// worse than one that erases late.</para>
    /// </summary>
    private static void ReleaseAccountScopedStoresBeforeErase()
    {
        var app = Current as App;

        // (1) `mls.db`'s remaining holder — the conversations SESSION.
        // `DropActorScopedState` ran first and disposed the outgoing MANAGER
        // (`ConversationsManagerHost.ResetForActorChange`), but the session holds an
        // `Arc<FaunaMlsBackend>` of its own and the role lock survives while EITHER
        // lives — that is the explicit contract in `RetireConvSession`'s doc and in
        // the host's. Only `DisposeNestClients` retired the session, and sign-out
        // does not call it (its clients outlive the wipe), so the one path that
        // erases without disposing was also the one path that erased with the store
        // still open. Measured: the sweep failed at
        // `<flat base>\<actor>` with `os error 32` while the C# side reported only a
        // swallowed WARN. Idempotent — the reset/logout arms already nulled these.
        if (app is not null)
        {
            RetireConvSession(null, app._liveConvSession);
            app._liveConvSession = null;
#if DEBUG || FAUNA_E2E_AGENT
            // The e2e login path never reaches `StartMainAppAsync`, so under a test
            // build THIS is the pair actually holding the engine.
            RetireConvSession(app._e2eConvSessionTask, app._e2eRealConvSession);
            app._e2eConvSessionTask = null;
            app._e2eRealConvSession = null;
#endif
        }

        // (2) the unified account store's holder — the account runtime — and the
        // conversations engine's own hand-over.
        //
        // ⚠ (1) above is NOT sufficient and never was, which is why this exists.
        // Disposing the C# session wrappers drops C# references; the Rust client
        // keeps its OWN stashes (`scheduling_session`, `index_arm`), so the engine,
        // its SQLite connection and its role lock stayed open through the erase no
        // matter how carefully this method nulled fields. The release below is
        // refcount-independent — it hands the role over explicitly rather than
        // hoping the last pointer has gone — and lives in shared Rust so apple and
        // android inherit it. On POSIX the same live engine went unnoticed because
        // `unlink` deletes an open file, so Windows is the only platform that ever
        // reports this.
        try
        {
            var stops = Core.Services.AccountRuntimeStops.Process;
            // Sign-out-shaped, not the switch stop: the only two callers that
            // reach this branch with a still-live `_rpcClient` are the real
            // sign-out (`SignOutHandler`) and the unreadable-index "start over"
            // path, and both run the sign-out routine's wipe right after this
            // returns (`FinishSignOut`) — the case `StopAccountRuntimeForSignOutAsync` exists for
            // (`NestRpcClient.StopAccountRuntimeForSignOutAsync`'s own doc). The
            // e2e `reset`/`logout` arms never reach this `if`: they call
            // `DisposeNestClients` first, which already nulls `_rpcClient` and
            // tracks its own (separately classified) stop.
            if (app?._rpcClient is { } live)
            {
                stops.Track(live.StopAccountRuntimeForSignOutAsync());
                stops.Track(live.ReleaseAccountScopedStoresAsync());
            }
            // Blocking is this method's contract (its callers erase on the next
            // line, synchronously); the barrier never throws, reports its own
            // lapse, and every await inside it is ConfigureAwait(false).
            stops.BarrierAsync("sign-out").GetAwaiter().GetResult();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[account-runtime] stop-before-erase failed: {ex.Message}");
        }
    }

    /// <summary>
    /// The e2e reset/logout arms' wipe: the WHOLE credential namespace (every
    /// account's per-actor slots and the index, <c>long-term-store.md</c> § Cleanup
    /// contract) and every account's scoped stores, with no sign-out record and no
    /// residue record. Delete-only, so a crash mid-wipe cannot resurrect the
    /// identity being erased.
    ///
    /// <para>Not the sign-out's erase any more: a real sign-out and the
    /// unreadable-index start-over record their decision first and run the one
    /// sign-out routine (<see cref="FinishSignOut"/>). These arms are a test
    /// boundary, not a journey a user watches, so what they leave is discarded —
    /// a record here would make every test's own reset look like a failed sign-out
    /// at the next relaunch.</para>
    /// </summary>
    private static void ClearCredentialNamespace()
    {
        // ⚠ FIRST, before anything is erased — see the method's own doc.
        ReleaseAccountScopedStoresBeforeErase();

        using (var registry = CredentialStore.Registry())
        {
            registry.ClearAll();
        }
        ForgetHandoffsOfWipedIdentities();

        // Erasure follows scope (account-scoping.md § Erasure follows scope): a
        // reset that wiped only the credential namespace would leave every
        // account's MLS store, drafts and backup state readable on disk. Drop
        // every scoped store; install-scoped state (logs, host-keyed pin store,
        // config-replica, app settings) is unnamed and survives.
        Core.Services.AccountStateDir.EraseAll();
        ActiveActorHex = null;
        // The window serves no account now: put down the process holder's
        // instance lock and its shared-root serving lock, AFTER the erase.
        Core.Services.SessionInstance.Release();
    }

    /// <summary>
    /// <b>The sign-out's tail</b> — the gesture's call of the one sign-out routine
    /// (<c>account-scoping.md</c> § Erasure follows scope → <i>The sign-out record is
    /// on all 7 apps</i>, decision 3), over the record <see cref="SignOutHandler"/>
    /// (or the unreadable-index start-over) wrote at its top. Blocking: run it off
    /// the UI thread where a window is up.
    ///
    /// <para>The runtime's stop first (<see cref="ReleaseAccountScopedStoresBeforeErase"/>
    /// — an open store is an unerasable store on windows), then the routine: the
    /// serving question, the wipe, each recorded account's scopes and auxiliary
    /// slots, the residue re-sweep, the residue record written before the sign-out
    /// record clears. The sync agent is not the routine's to dismiss here — this
    /// gesture's own <c>AwaitUnprovisionThenErase</c> awaited its un-provision
    /// already. Then the window serves no account: the process holder's instance
    /// lock and its shared-root serving lock go down AFTER the erase — a holder kept
    /// past here made this signed-out window a "sibling" to every later sign-out of
    /// the account it left (§ Concurrent instances → <i>An erase refuses while a
    /// sibling serves the account</i>).</para>
    /// </summary>
    /// <returns>The <c>sign-out-residue</c> view to paint — what the run left, or the
    /// refusal line when another window began serving a recorded account between
    /// the page's <c>SignOutBlocked</c> and this run (the record then stands for the
    /// next launch); <c>null</c> when clean.</returns>
    private static Core.Services.ISignOutResidueSurface? FinishSignOut(string[]? written)
    {
        // ⚠ FIRST, before anything is erased — ReleaseAccountScopedStoresBeforeErase's
        // own doc.
        ReleaseAccountScopedStoresBeforeErase();
        ForgetHandoffsOfWipedIdentities();

        Core.Services.ISignOutResidueSurface? residue = null;
        try
        {
            residue = Core.Services.SignOutResidueSurface.Finish(
                SignOutResidueSeat, uniffi.fauna_ffi.FfiFinishCaller.Gesture, written).Residue;
        }
        catch (Exception ex)
        {
            // Said nothing about the disk: the record stands, and the next launch
            // finishes what this run did not.
            ShellLog.Warn("App", $"[sign-out] the gesture's finish failed: {ex.Message}");
        }
        ActiveActorHex = null;
        Core.Services.SessionInstance.Release();
        return residue;
    }

    /// <summary>
    /// The handoffs a credential wipe leaves nobody to run as. The ONE clear point
    /// for what a succession hands across its own account switch
    /// (<c>identity-succession.md</c> § The RecoveryKey → At succession). ⚠ Not the
    /// switch teardown, deliberately: those fields belong to the OUTGOING identity's
    /// ceremony and the teardown they have to survive IS that ceremony's closing
    /// act. A wipe is different — it destroys every identity on the box, so there is
    /// no successor left to owe a kit to and no predecessor row left for that kit to
    /// seal; and a sign-in follow-up queued for an identity the wipe destroys has
    /// nobody left to run as.
    /// </summary>
    private static void ForgetHandoffsOfWipedIdentities()
    {
        Core.Services.SuccessionHandoff.ClearOnCredentialWipe();
        Core.Services.PostSignInHandoff.Clear();
    }

    /// <summary>
    /// Where this seat's sign-out erases — the arguments
    /// <c>AccountStateDir.EraseAll</c> and <c>SignOutBlocked</c> are called with
    /// (the install base, and the shared per-user store root as a <c>null</c>
    /// container) — plus the one credential store's registry view. What the one
    /// sign-out routine needs to record, finish and re-sweep.
    /// </summary>
    private static Core.Services.ResidueSeat SignOutResidueSeat =>
        new(Core.Services.AccountStateDir.Base, null, CredentialStore.Registry);

    /// <summary>
    /// <b>The launch's call of the one sign-out routine</b>, before anything reads
    /// the registry (<c>account-scoping.md</c> § Erasure follows scope → <i>The
    /// sign-out record is on all 7 apps</i>, decision 3): a sign-out a dead process
    /// recorded is finished — the sync agent dismissed from each recorded account,
    /// the wipe, each account's erase — and the residue record re-swept silently
    /// while nobody is signed in. Nothing of this process is open yet, so there is
    /// no store to release and no lock to put down. Off the UI thread: file I/O and
    /// up to the agent's budget per recorded account.
    ///
    /// <para>A finish that throws is logged and launches over what is there — the
    /// records still stand for the next launch.</para>
    /// </summary>
    private static async Task<Core.Services.SignOutFinishOutcome> FinishAtLaunchAsync()
    {
        var seat = SignOutResidueSeat;
        try
        {
            return await Task.Run(() => Core.Services.SignOutResidueSurface.Finish(
                seat, uniffi.fauna_ffi.FfiFinishCaller.Launch, null));
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[sign-out] the launch's finish failed ({ex.Message}) — launching over what is there");
            return Core.Services.SignOutFinishOutcome.Nothing;
        }
    }

    /// <summary>
    /// What the launch's sign-out finish (<see cref="FinishAtLaunchAsync"/>) left to
    /// paint, until the cold launch's first <c>identity_choice</c> takes it — the
    /// one landing it was reported to. Cleared once the cold launch's dispatch
    /// returns, so a later landing never paints a launch's stale line.
    /// </summary>
    private Core.Services.ISignOutResidueSurface? _launchSignOutResidue;

    private Core.Services.ISignOutResidueSurface? TakeLaunchSignOutResidue() =>
        System.Threading.Interlocked.Exchange(ref _launchSignOutResidue, null);

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// Whether a session-patch <c>device_id</c> is sync-shaped (32-byte hex) —
    /// the ONE case the e2e session door adopts verbatim rather than deriving
    /// (sync-agent-credentials.md § Implementation status today: "the e2e
    /// session door's forced id keeps working unchanged"). Anything else,
    /// including the un-forced sentinel (<c>helpers/enrollment.py</c>'s
    /// <c>UNFORCED_DEVICE_ID</c>), is not a device id this store may ever
    /// register a row under — never adopted. The session door is the test
    /// command handler below, so this compiles out of Release with it.
    /// </summary>
    private static bool IsSyncShapedDeviceId(string id)
    {
        try { return Convert.FromHexString(id).Length == 32; }
        catch (FormatException) { return false; }
    }
#endif

    /// <summary>
    /// The admin auto-default (Stage 2, <c>long-term-store.md</c> § Per-account re-auth):
    /// flip <c>require_confirm_to_activate</c> ON for the <b>active</b> account iff the user
    /// has never touched that account's toggle (<c>require_confirm_user_set</c> unset). The
    /// shared <c>auto_enable_require_confirm</c> is idempotent and never turns the flag off,
    /// so this is safe to call on every <c>am-i-admin = true</c> observation.
    ///
    /// <para>Called from <see cref="Views.MainPage.CheckAdminStatusAsync"/> — the client
    /// decides when an account counts as admin (the nav gate that reveals <c>admin-tab</c>);
    /// the registry never learns admin-ness. Resolves the active actor itself, mirroring
    /// apple's <c>FaunaAccounts.autoEnableRequireConfirmForActiveAdmin</c>.</para>
    ///
    /// <para><b>Best-effort:</b> a failure here must never break the admin gate, so it is
    /// logged and swallowed (an explicit user OFF still sticks — the marker is what protects
    /// it, not this call).</para>
    /// </summary>
    internal static void AutoEnableRequireConfirmForActiveAdmin()
    {
        try
        {
            using var registry = CredentialStore.Registry();
            var actorId = registry.Active();
            if (actorId is null)
            {
                return;
            }
            if (registry.AutoEnableRequireConfirm(actorId))
            {
                ShellLog.Info("App", $"[reauth] auto-enabled require-confirm for admin {actorId}");
            }
        }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[reauth] admin auto-default skipped: {ex.Message}");
        }
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// Handle a test command. Does fast state changes synchronously and returns
    /// an optional post-action for deferred navigation (runs after state is pushed).
    ///
    /// <para>Several blocks of ONE <c>set_state</c> may each need UI-thread work
    /// (<c>session</c> + <c>nav</c> + <c>messages</c> + <c>compose</c> ride together).
    /// Every block therefore <b>composes</b> onto <c>navAction</c> with
    /// <see cref="PostActionChain.Then"/> and none may overwrite it — see that type
    /// for why a clobbering <c>nav</c> block left <c>MainPage</c> holding disposed
    /// clients on every second-or-later login.</para>
    /// </summary>
    private Func<Task>? HandleTestCommand(Dictionary<string, object?> command, ISessionAccount account, Frame rootFrame, string? closuredNestUrl)
    {
        if (command.TryGetValue("__action", out var action))
        {
            switch (action?.ToString())
            {
                case "reset":
                    // Sign-out-shaped: the erase below (moved into the returned
                    // post-action, which PollLoopAsync awaits before replying —
                    // TestAgent.cs's `await postAction()`) takes the account-store
                    // slot (the writer key) with it — same rule as the real sign-out
                    // and "start over" paths (ReleaseAccountScopedStoresBeforeErase's
                    // own classification). DisposeNestClients stops the sync-agent
                    // loop synchronously here (`StopSyncAgentSession`); the erase
                    // itself now waits for the agent's un-provision reply first, the
                    // same order the real sign-out path runs in — this arm never un-provisioned at all before, unlike
                    // the three teardown paths DisposeNestClients's own doc names.
                    //
                    // ⚠ The un-provision STARTS before DisposeNestClients, whose
                    // StopSyncAgentSession would otherwise drop the session first and
                    // leave it nothing to tear down: the agent then kept its account
                    // runtime — and account-store.db — open, and every reset's erase
                    // failed with os error 32 (ErasePrecondition.BeginUnprovisionThenTearDown
                    // owns the why).
                    //
                    // A launch still in flight is retired first, as the real sign-out
                    // does: a launch surface is launch-scoped, never test-scoped, so a
                    // verdict the previous test left in flight must not paint over the
                    // next one (Core.Services.LaunchCurrency).
                    Core.Services.LaunchCurrency.Shared.RetireInFlight();
                    var eraseAfterUnprovision = ErasePrecondition.BeginUnprovisionThenTearDown(
                        HydrationSessionEnabled, UnprovisionSyncAgentAsync,
                        () => DisposeNestClients(signOut: true),
                        () => ClearCredentialNamespace());
                    // The state machine is in-memory only — there's no
                    // wizard scratchpad to wipe; the next launch starts at
                    // identity_choice automatically once the long-term store
                    // is cleared.
                    _cryptoService = new CryptoService();
                    _handle = null;
                    _deviceId = null;
                    _testCurrentView = "welcome";
                    // Drop the set_state authentication override, so the flag falls
                    // back to the derived mounted-fact — the postAction below
                    // re-roots onboarding, making it false. Mirrors linux's
                    // `s.session_override = None` on this same action.
                    _testAuthenticatedOverride = null;
                    _currentErrorMessage = null;
                    _injectedErrorMessage = null;
                    // The ONE clear of the agent-failure slot (convention 11), and the
                    // reason it may not be cleared anywhere else: `reset` is the per-test
                    // boundary every `app` fixture drives, so each test gets exactly one
                    // clean read and a refusal can never leak into a test that did not
                    // cause it. Note the two OTHER `_currentErrorMessage = null` sites in
                    // this method (the `session` login block and the `nav` block) are
                    // precisely why the page mirror cannot hold a refusal at all: they run
                    // on every login and every `navigate_to`. Mirrors linux's
                    // `s.agent_command_failure = None` on this same action.
                    _agentCommandFailure = null;
                    // A barrier probe token is scoped to ONE test — same clear point
                    // as tui's `App::barrier_probe` and linux's `clear_barrier_probe`.
                    ClearBarrierProbes();
                    // The same canonical drop the production paths take (reached
                    // through DisposeNestClients above too — idempotent). Only the
                    // e2e-ONLY extra stays hand-listed at this site, per the rule in
                    // DropActorScopedState's header.
                    DropActorScopedState();
                    // Session field, nulled at each teardown site per that same header —
                    // the production sign-out, switch and factory-reset arms do too. The
                    // observer is bound to the manager DropActorScopedState just retired,
                    // and holding it would keep that manager alive until the next login.
                    _messageToastObserver = null;
                    try { FaunaApp.Conversations.ConversationsManagerHost.Instance.ClearForTest(); }
                    catch { /* test-helpers may be off in non-e2e builds */ }
                    // A reset app is a new device, and a new device is a new process
                    // with a whole dial budget. This process outlives the reset, so
                    // without the clear one test's journey spends the next test's
                    // burst and every dial after it waits 10 s — a successor's
                    // closing-act kit then never rendered inside its budget.
                    uniffi.fauna_ffi.FaunaFfiMethods.DialBudgetClearForTest();
                    return async () =>
                    {
                        // Await the un-provision before the erase (see the case's own
                        // comment above) — PollLoopAsync awaits this whole closure
                        // before marking the command ready, so the erase is complete
                        // before the harness's next command can race it.
                        await eraseAfterUnprovision();
                        // Close any ContentDialog a prior test left open (e.g. the create
                        // wizard) BEFORE navigating away, so the next test starts from a
                        // clean XamlRoot (Controls.Dialogs owns the registry and the why).
                        // MUST run here (the postAction, UI thread): Hide() is a UI-thread
                        // op, and HandleTestCommand itself runs on the agent poll thread.
                        Controls.Dialogs.CloseAll();
                        var navParam = new ServiceClients(
                            _nestClient ?? new DirectNestClient(closuredNestUrl ?? $"https://127.0.0.1:{DefaultNestPort}", _cryptoService),
                            _cryptoService, account,
                            OnOnboardingCompleted: _onOnboardingCompleted);
                        rootFrame.Navigate(typeof(Views.OnboardingPage), navParam);
                    };
                case "logout":
                    // Sign-out-shaped, same reasoning — and the same un-provision-first
                    // ordering — as the "reset" arm above, the launch retirement too.
                    Core.Services.LaunchCurrency.Shared.RetireInFlight();
                    var logoutEraseAfterUnprovision = ErasePrecondition.BeginUnprovisionThenTearDown(
                        HydrationSessionEnabled, UnprovisionSyncAgentAsync,
                        () => DisposeNestClients(signOut: true),
                        () => ClearCredentialNamespace());
                    // No wizard scratchpad to wipe — state machine is in-
                    // memory only.
                    _cryptoService = new CryptoService();
                    _handle = null;
                    _deviceId = null;
                    _testCurrentView = "welcome";
                    // Same clear as `reset` above — linux handles both actions in
                    // one `"reset" | "logout"` arm for exactly this reason.
                    _testAuthenticatedOverride = null;
                    _currentErrorMessage = null;
                    _injectedErrorMessage = null;
                    // Same one-test lifetime as the `reset` arm above, and the same
                    // "only here" rule (convention 11).
                    _agentCommandFailure = null;
                    // Same one-test probe-token lifetime as the `reset` arm above.
                    ClearBarrierProbes();
                    // Same canonical drop as the `reset` arm above.
                    DropActorScopedState();
                    // Same session-field null as the `reset` arm above.
                    _messageToastObserver = null;
                    try { FaunaApp.Conversations.ConversationsManagerHost.Instance.ClearForTest(); }
                    catch { /* test-helpers may be off in non-e2e builds */ }
                    return async () =>
                    {
                        // Same ordering as the "reset" arm above — the erase waits
                        // for the un-provision reply first.
                        await logoutEraseAfterUnprovision();
                        var navParam = new ServiceClients(
                            _nestClient ?? new DirectNestClient(closuredNestUrl ?? $"https://127.0.0.1:{DefaultNestPort}", _cryptoService),
                            _cryptoService, account,
                            OnOnboardingCompleted: _onOnboardingCompleted);
                        rootFrame.Navigate(typeof(Views.OnboardingPage), navParam);
                    };
            }
            return null;
        }

        // Composed, never overwritten — each block below appends with .Then(...).
        Func<Task>? navAction = null;

        if (command.TryGetValue("session", out var sessionObj) && sessionObj is System.Text.Json.JsonElement sessionEl)
        {
            var session = System.Text.Json.JsonSerializer.Deserialize<Dictionary<string, System.Text.Json.JsonElement>>(sessionEl.GetRawText());
            if (session != null)
            {
                // An authenticated patch IS the session now, so a launch still in
                // flight is superseded: retire it, and its verdict is dropped when it
                // lands rather than rendered over the session built below
                // (Core.Services.LaunchCurrency; linux's and tui's session patches
                // retire at the same point). Synchronous, in the command body, so the
                // retirement precedes every state read the patch's ack unblocks.
                if (session.TryGetValue("authenticated", out var supersedes)
                    && supersedes.ValueKind == System.Text.Json.JsonValueKind.True)
                {
                    Core.Services.LaunchCurrency.Shared.RetireInFlight();
                }
                // Who was signed in before this patch — read BEFORE the secret arm
                // below replaces the crypto service, so the re-point can tell a
                // same-actor re-establish (the identity lives on) from a change of
                // identity (it ends).
                var priorActor = _cryptoService is { HasKey: true } prior ? prior.ActorIdHex : null;
                // The session patch is a REGISTRY write, exactly the shape a real
                // sign-in leaves behind (linux's session door, main.rs): the secret
                // arm enrolls the identity (`AddAccount`, an idempotent upsert) and
                // moves `active` to it (`SetActive`), and every other arm writes the
                // served account's own per-actor row. On an actor switch the pointer
                // move is load-bearing: a launch machine rebuilt over
                // `LaunchPersistence()` resolves `active`, so leaving it on the
                // OUTGOING actor would pair actor A's bearer with actor B's client —
                // a pairing the nest refuses at the WS handshake. A test-injected
                // identity is never re-auth-flagged, so plain SetActive is right.
                // Failures are logged, never thrown: the agent reply must still land.
                void OnServedAccount(string what, Action<uniffi.fauna_ffi.FfiAccountRegistry, string> write)
                {
                    if (_cryptoService is not { HasKey: true } served) return;
                    try
                    {
                        using var registry = CredentialStore.Registry();
                        write(registry, served.ActorIdHex);
                    }
                    catch (Exception ex)
                    {
                        ShellLog.Error("App", $"[cmd-session] {what} failed: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                if (session.TryGetValue("secret_hex", out var secret) && secret.ValueKind == System.Text.Json.JsonValueKind.String)
                {
                    var hex = secret.GetString()!;
                    _cryptoService = new CryptoService();
                    _cryptoService.LoadFromSecret(hex);
                    OnServedAccount("registry enrol", (registry, _) =>
                        registry.SetActive(registry.AddAccount(hex, null, null)));
                }
                if (session.TryGetValue("node_url", out var url) && url.ValueKind == System.Text.Json.JsonValueKind.String)
                {
                    var nodeUrl = url.GetString()!;
                    OnServedAccount("nest_url write", (registry, actor) => registry.SetNestUrl(actor, nodeUrl));
                    // Re-pointing at a (possibly different) nest: dispose the prior
                    // clients first, else the old WS-RPC client leaks (see
                    // DisposeNestClients). No-op when reset/logout already cleared them.
                    // Switch-shaped (the default): a nest re-point never erases the
                    // credential namespace, so the account-store slot survives —
                    // same rule as the account-switch handler above.
                    // Same actor as before the patch → the identity lives on, and
                    // so do its standing critical alerts (critical-alerts.md §
                    // Mechanism → Lifetime); only a change of identity drops them.
                    var sameIdentity = priorActor is not null
                        && _cryptoService is { HasKey: true } now
                        && string.Equals(priorActor, now.ActorIdHex, StringComparison.OrdinalIgnoreCase);
                    Core.Logs.E2eTrace.Write($"[cmd-session] before DisposeNestClients (sameIdentity={sameIdentity})");
                    DisposeNestClients(sameIdentity: sameIdentity);
                    Core.Logs.E2eTrace.Write("[cmd-session] after DisposeNestClients; building clients");
                    _nestClient = new DirectNestClient(nodeUrl, _cryptoService!);
                    _rpcClient = NewRpcClient(nodeUrl, _cryptoService!);
                    Core.Logs.E2eTrace.Write("[cmd-session] clients built");
                }
                if (session.TryGetValue("handle", out var handleEl) && handleEl.ValueKind == System.Text.Json.JsonValueKind.String)
                {
                    _handle = handleEl.GetString();
                    if (_handle is string patchedHandle)
                    {
                        // The index entry's cache: handle replaced, domain/tier kept.
                        OnServedAccount("handle cache write", (registry, actor) =>
                        {
                            var entry = registry.List().FirstOrDefault(e =>
                                string.Equals(e.@actorId, actor, StringComparison.OrdinalIgnoreCase));
                            registry.UpdateCache(actor, patchedHandle, entry?.@domain, entry?.@tier);
                        });
                    }
                }
                if (session.TryGetValue("device_id", out var deviceEl) && deviceEl.ValueKind == System.Text.Json.JsonValueKind.String)
                {
                    var injected = deviceEl.GetString();
                    if (injected is not null && IsSyncShapedDeviceId(injected))
                    {
                        // The forced-id path (sync-agent-credentials.md § Implementation
                        // status today: "the e2e session door's forced id keeps working
                        // unchanged") — kept, and never persisted through the derivation:
                        // written straight into the served account's per-actor slot
                        // (`AddAccount` re-upserts the row, touching only the slot given).
                        _deviceId = injected;
                        OnServedAccount("device_id write", (registry, actor) =>
                        {
                            if (registry.SessionMaterial(actor) is { } material)
                                registry.AddAccount(material.@secretHex, null, injected);
                        });
                    }
                    else
                    {
                        // An injected id that is NOT 32-byte hex (the e2e un-forced case,
                        // helpers/enrollment.py's UNFORCED_DEVICE_ID) falls through to the
                        // derivation instead of being adopted verbatim — apple's fixed
                        // shape (§ Credential model, the RULED 2026-09-20 block), and the
                        // same call OnboardingViewModel's LoggedIn writer makes. Resolves
                        // the actor from THIS patch's own crypto service, since the
                        // secret_hex arm above (when present in the same patch) just
                        // rebuilt it. The derivation persists the id into the account's
                        // own slot itself (the account is registered by then).
                        OnServedAccount("deviceIdForActor", (registry, actor) =>
                            _deviceId = registry.DeviceIdForActor(CredentialStore.Logical, actor));
                    }
                }
                // Record the override FIRST, and for either value — `false` is a
                // meaningful patch, not an absent one (linux stores the same
                // `Option<bool>`). SerializeState prefers it over the derived
                // mounted-fact, so a login patch reads `true` immediately even
                // though the MainPage hand-off below is a deferred post-action.
                if (session.TryGetValue("authenticated", out var authPatch)
                    && authPatch.ValueKind is System.Text.Json.JsonValueKind.True
                        or System.Text.Json.JsonValueKind.False)
                {
                    _testAuthenticatedOverride = authPatch.GetBoolean();
                }
                if (session.TryGetValue("authenticated", out var auth) && auth.GetBoolean())
                {
                    _testCurrentView = "feed";  // default view after login
                    _currentErrorMessage = null;
                    _injectedErrorMessage = null;
                    _currentWarningMessage = null;
                    _currentInfoMessage = null;
                    if (_nestClient != null && _cryptoService != null)
                    {
                        var client = _nestClient;
                        var crypto = _cryptoService;
                        var rpc = _rpcClient;
                        navAction = navAction.Then(async () =>
                        {
                            // Dismiss a ContentDialog the PREVIOUS actor left open (e.g. a
                            // feed post-detail) before navigating this login in. A dialog is
                            // owned by the XamlRoot, not the frame, so it survives the
                            // Navigate below and would both cover the new actor's page and
                            // make their next open refused (one dialog per XamlRoot).
                            // Same close the `reset` postAction does, and the same UI-thread
                            // requirement — this postAction runs on the UI thread, the
                            // command body does not.
                            Controls.Dialogs.CloseAll();
                            var conv = await AwaitE2eConvSessionAsync();
                            // Draft-persistence v2, feed leg: bounded-await the sync
                            // handle kicked off below BEFORE navigating, since
                            // FeedPage — the default post-login view — reads
                            // App.FeedDraftsSync in its own Page_Loaded.
                            Core.Logs.E2eTrace.Write("[cmd-session] awaiting AwaitE2eFeedDraftsSyncAsync");
                            await AwaitE2eFeedDraftsSyncAsync();
                            Core.Logs.E2eTrace.Write($"[cmd-session] AwaitE2eFeedDraftsSyncAsync returned, FeedDraftsSync={(FeedDraftsSync is null ? "null" : "set")}");
                            // Reconnect pump for the e2e login — the same
                            // set_state-never-reaches-StartMainAppAsync gap the push pump
                            // and the drafts sync are promoted for in the command body
                            // above, and the one that made the benign-flip feed re-hydrate
                            // untestable on windows. `StartReconnectPump` drains
                            // `FfiNestClient.SubscribeReconnects` and raises
                            // `INestRpcClient.Reconnected` — the ONLY trigger the live
                            // surfaces' `ViewModelBase.RefreshOnReconnect` re-fetch hangs
                            // off (transport.md § Push events: the feed has no poll
                            // backstop, so a post that arrived while disconnected reaches
                            // the list only through that re-fetch). Measured before this
                            // call existed: across a full nest flip the app raised
                            // `Reconnected` exactly zero times and ran zero feed reloads,
                            // ending on precisely its pre-flip post list — so
                            // `test_nest_flip_feed_rehydrate[windows]` was asserting
                            // against a mechanism that had never been started. This is the
                            // windows twin of the macOS defect
                            // `e2e-latency-independent-assertions.md` records under this
                            // same counter's rollout ("the reconnect observer the feed's
                            // re-hydrate hook depends on was never promoted for the
                            // in-process e2e agent's explicit wiring block")
                            // .
                            //
                            // ⚠ HERE, not in the command body beside its two siblings, and
                            // the difference is load-bearing: this postAction runs on the
                            // UI thread and the command body does not (see the stale-dialog
                            // note at the top of this closure). `StartReconnectPump`
                            // captures `SynchronizationContext.Current` as the context
                            // `RaiseReconnectedOnUi` marshals the event through, so started
                            // from the command body's thread-pool thread it would deliver
                            // `Reconnected` OFF the UI thread — straight into bound
                            // view-model state, the WinUI COMException trap. It assigns
                            // that field unconditionally (unlike `StartPushPump`'s `??=`),
                            // so a pool-thread call would also clobber the context its
                            // siblings marshal through. Idempotent, so production's own
                            // `StartMainAppAsync` call is unaffected.
                            rpc?.StartReconnectPump();
                            Core.Logs.E2eTrace.Write("[cmd-session] after StartReconnectPump (UI thread)");
                            // The identity follow too: this login reaches no launch
                            // machine, so the session's own mint is the cache's only
                            // writer here (the handle the patch wrote carries no domain
                            // or tier) — FaunaKit's e2e session patch starts it the same.
                            rpc?.StartSelfIdentityFollow();

                            var navParam = new ServiceClients(client, crypto, account, conv, Rpc: rpc);
                            rootFrame.Navigate(typeof(Views.MainPage), navParam);
                        });
                        // Build the real ConversationsSession for this e2e login. Kicked
                        // off HERE — directly on authentication, NOT inside navAction —
                        // because the build is independent of navigation and starting it
                        // before the deferred post-action runs gives it a head start on
                        // the bounded await below. (It also used to be load-bearing: a
                        // sibling `nav` block overwrote navAction outright. It no longer
                        // does — the blocks compose via PostActionChain.Then — but the
                        // early kickoff is still the right shape.) Both nav
                        // actions then AWAIT it (bounded) so the pages receive it via
                        // ServiceClients.ConvSession, the same way production's
                        // StartMainAppAsync hands it over. Mirrors linux
                        // conv_backend::start_conversations_session, which likewise runs
                        // the real session under e2e for EVERY login. Nothing here mutates
                        // WinUI bound state, so the off-UI-thread kickoff is safe.
                        if (rpc != null)
                        {
                            Core.Logs.E2eTrace.Write("[cmd-session] kicking off BuildE2eConvSessionAsync");
                            // Release the OLD task before overwriting the field: a repeat
                            // `session` patch (no intervening DisposeNestClients()) would
                            // otherwise drop the only reference able to dispose whatever a
                            // still-in-flight predecessor eventually builds. The session
                            // itself is released inside BuildE2eConvSessionAsync, above —
                            // this is the task half that seam does not reach.
                            RetireConvSession(_e2eConvSessionTask, null);
                            // The set_state patch above already persisted this login's
                            // device_id, so the store is the same source production reads.
                            _e2eConvSessionTask = BuildE2eConvSessionAsync(rpc, account.ResolveDeviceId());
                            Core.Logs.E2eTrace.Write("[cmd-session] BuildE2eConvSessionAsync kicked off (returned to caller)");
                        }

                        // Host the W3 account-store runtime for this e2e login too (the
                        // set_state path never reaches StartMainAppAsync, where production
                        // starts it) — independent of the conversations session above, same
                        // reasoning as production's call there. Fire-and-forget: best-effort
                        // by design (StartAccountRuntimeAsync catches internally), and
                        // test_account_runtime_pump.py deadline-polls the assembly rather
                        // than requiring it complete before this command acks. Android and
                        // macOS each hit the identical trap by considering only their
                        // conversations-session call site, which also returns early under
                        // e2e (`account-data-plane.md` § Implementation status today).
                        if (rpc is not null)
                        {
                            _ = rpc.StartAccountRuntimeAsync(Core.Services.AccountStateDir.Base, account.ResolveDeviceId());
                            // Same gap for the mail connect-time refresh: the set_state
                            // path never reaches StartMainAppAsync's start. Idempotent per
                            // client, never throws (macOS makes the same second call).
                            _ = Core.Helpers.MailEpochSchedule.StartAsync(rpc);
                        }

                        // Author-side subscriptions reconciliation for the e2e login
                        // (the set_state path never reaches StartMainAppAsync, where
                        // production starts it). Unconditional — unlike the real-
                        // conversations loop it has no mock twin to preserve, and it
                        // only reads/drains this author's own queue (auto_approve
                        // tiers only), so deterministic suites are unaffected.
                        // Mirrors linux, which runs its real session under e2e for
                        // every login. monetization.md § The unifying model, path 2.
                        rpc?.StartSubscriptionsAuthorPump();
                        Core.Logs.E2eTrace.Write("[cmd-session] after StartSubscriptionsAuthorPump");

                        // Generic push pump for the e2e login (the set_state path never
                        // reaches StartMainAppAsync, where production starts it) — without
                        // this, test_push_live_refresh.py's mounted-notifications-page
                        // probe never converges: the client receives the push frame but has
                        // no dispatch loop draining FfiNestClient.SubscribePushes() to raise
                        // it. Unconditional, same rationale as StartSubscriptionsAuthorPump
                        // above (a no-op drain until a real push arrives).
                        rpc?.StartPushPump();
                        Core.Logs.E2eTrace.Write("[cmd-session] after StartPushPump");

                        // Dedicated knock pump for the e2e login (the set_state path never
                        // reaches StartMainAppAsync, where production starts it) — without
                        // this, test_knock_live_refresh.py's mounted-contacts-page probe
                        // never converges: fauna.knock is its own SubscribeKnocks stream
                        // (not routed through the generic push pump above), so ContactsPage's
                        // KnockReceived subscription has no pump ever raising it. Same
                        // rationale as StartPushPump above (a no-op drain until a real knock
                        // arrives).
                        rpc?.StartKnockPump();
                        Core.Logs.E2eTrace.Write("[cmd-session] after StartKnockPump");

                        // …and the knock OS toast production hangs on that pump
                        // (StartMainAppAsync's KnockReceived += ShowKnockNotification).
                        // notifications outcome 15's witness
                        // (test_notifications_knock_toast.py) reads the log
                        // ShowKnockNotification feeds; without this the pump raised each
                        // knock to the Contacts roster alone and the app logged `seen: 0`.
                        // `-=` first: a repeat `session` patch on a surviving client must
                        // not stack a second handler (one toast per knock). The toast host
                        // is initialised here for the same reason as BuildE2eConvSessionAsync
                        // does it (idempotent).
                        if (rpc is not null)
                        {
                            NotificationService.Initialize();
                            rpc.KnockReceived -= NotificationService.ShowKnockNotification;
                            rpc.KnockReceived += NotificationService.ShowKnockNotification;
                        }

                        // Admin host-address report for the e2e login (the set_state path
                        // never reaches StartMainAppAsync, where production runs it) — the
                        // never-publish-a-private-address witness
                        // (test_admin_dns_host_address_lan.py) reads the shared fn's
                        // outcome line off the app's stderr. am_i_admin-gated like
                        // production, and safe on any test nest: STUN is deferred, so a
                        // loopback or LAN dial reports nothing. Mirrors linux, which runs
                        // its real session under e2e for every login.
                        if (rpc is not null) _ = new HostAddressReporter(rpc).RunAsync();

                        // Draft-persistence v2, feed leg (reserved-folders.md § Drafts
                        // Sync): build the nest-backed __drafts autosync (rail "posts")
                        // for the e2e login too (the set_state path never reaches
                        // StartMainAppAsync, where production wires it). Kicked off HERE,
                        // same shape as _e2eConvSessionTask above — the navAction closure
                        // below bounded-awaits it before navigating, since FeedPage (the
                        // default post-login view, _testCurrentView = "feed") reads
                        // App.FeedDraftsSync in Page_Loaded and a race would silently
                        // skip restore for that first navigation.
                        if (rpc is not null) _e2eFeedDraftsSyncTask = WireFeedDraftsAsync(rpc);

                        // Draft-persistence v2, events leg (reserved-folders.md § Drafts
                        // Sync): same "the set_state path never reaches StartMainAppAsync"
                        // gap as the feed line above. Unlike feed, Events is not the
                        // default post-login view, so there is no single navAction choke
                        // point to bound-await this from here — EventsPage's own first
                        // load bound-awaits EventDraftsSyncTask directly instead (see its
                        // own WireEventDraftsAsync).
                        if (rpc is not null) EventDraftsSyncTask = WireEventDraftsAsync(rpc);

                        // critical-alerts.md § Mechanism → Who runs the detector: the
                        // session-start sweep rides the SAME universal post-auth hook as
                        // the pumps above in production (StartMainAppAsync) — the set_state
                        // path never reaches it, so without this call every e2e login
                        // (first AND re-auth) never ran the sweep. Found via
                        // test_alert_sweep_directory_feeders_e2e.py's feeder #3 (sweep-
                        // driven, no settings-page nav): the alarm never appeared because
                        // nothing ever dispatched the sweep under e2e. Best-effort,
                        // fire-and-forget — the SAME call as StartMainAppAsync's, so the
                        // e2e build runs the product's loop: a first or identity-changing
                        // login starts the re-sweep LOOP (what `alert_sweep_wake` wakes).
                        // A same-identity re-auth — this handler re-fires on every e2e
                        // login in one process — disposed the previous client above
                        // (DisposeNestClients), which ended the loop riding it, so it
                        // starts the re-established loop on this new client: never a
                        // second loop on a live client, never a loop left on a dead one.
                        // (The temporary sweep-block traces that sat here came out once
                        // their regression was found and fixed, 2026-09-01: the repaint
                        // ran inline on the FFI callback thread — CriticalAlertsHost's
                        // _uiContext note.)
                        if (account.NestUrl is string sweepNestUrl && rpc is not null)
                        {
                            Core.Helpers.CriticalAlertsSweep.StartForIdentity(rpc, sweepNestUrl);
                        }
                        else
                        {
                            Core.Logs.E2eTrace.Write("[cmd-session] sweep SKIPPED — LoadNestUrl returned null, or no session client");
                        }

                        // The certificate auto-renew cadence (tls-certificates.md § C.3)
                        // rides the SAME universal post-auth hook in production
                        // (StartMainAppAsync), which this login never reaches. Without
                        // this call no e2e login would run the tick, and the
                        // `auto_renew_wake` poke (test_admin_dns_cert_auto_renew_tick.py)
                        // would have no loop to wake. The SAME call as production's: one
                        // cadence per client and actor generation, so a repeat `session`
                        // patch over the same client joins the live loop rather than
                        // stacking another. It sits on its 6 h wait until woken, so
                        // deterministic suites are unaffected. Mirrors linux, which runs
                        // the cadence for every e2e login.
                        if (rpc is not null) _ = RunAutoRenewCadenceAsync(rpc);

                        // succession-aftermath.md § Re-key scope's `BackupKey` corpus
                        // row: the aftermath rides the SAME universal post-auth hook as
                        // the passes above in production (StartMainAppAsync) — the
                        // set_state path never reaches it, so without this call an e2e
                        // login as a successor never re-keys its inherited corpus, and
                        // every post-succession leg times out on a blob still sealed to
                        // the retired identity.
                        //
                        // ⚠ The post-CEREMONY switch is already covered by the
                        // production call: SwitchAccountHandler rebuilds through
                        // DispatchLaunchSnapshotAsync, which routes Online →
                        // StartMainAppAsync. What this seam adds is every OTHER
                        // authenticated e2e start — above all a successor's SECOND
                        // sign-in, which is precisely the resumption case the pass is
                        // built for and the one no ceremony is around to notice.
                        //
                        // Unconditional and best-effort, same rationale as the pumps
                        // above: the pass returns NotASuccessor having done nothing for
                        // an identity that never succeeded, so it is a no-op drain for
                        // every deterministic suite.
                        if (rpc is not null)
                        {
                            Core.Logs.E2eTrace.Write("[cmd-session] dispatching succession aftermath");
                            _ = Core.Helpers.SuccessionAftermath.RunAsync(rpc);
                        }

                        // The once-per-sign-in newer-version look rides the SAME
                        // universal post-auth hook in production (StartMainAppAsync),
                        // which this login never reaches — so without this call no e2e
                        // sign-in would ever look. One look per login, on this login's
                        // own client: it reads the session nest's newest_release.
                        if (rpc is not null)
                            _ = Core.Services.UpdateCheck.Shared.LookOnceAtSignInAsync(rpc);

                        // Push session start — the announce and the opt-in-gated
                        // re-arm production runs in StartMainAppAsync, which this
                        // login never reaches. Best-effort, a no-op re-arm for every
                        // install that never opted in.
                        if (rpc is not null && crypto.HasKey)
                        {
                            _ = Core.Services.PushSession.OnSessionStartAsync(
                                rpc, crypto.ActorIdHex, account.ResolveDeviceId());
                        }
                        EnsureAgentAttachment();

                        // Session-scoped hydration provisioning, e2e seam: the SAME
                        // HydrationSessionService production starts in
                        // StartMainAppAsync. This is what makes the
                        // spawn→provision→hydration chain reachable under an e2e
                        // login at all (the set_state path never enters
                        // StartMainAppAsync). windows.md § On-demand hydration host.
                        //
                        // ⚠ It asks HydrationSessionEnabled rather than re-reading an
                        // env name, and that is the whole of the 2026-09-21 flip: the
                        // gate is "has the harness pinned an agent for us", not "did
                        // one suite opt in". Spelled as a second literal
                        // FAUNA_E2E_REAL_SYNC_AGENT read until then, this line was the
                        // reason a default `--app windows` run provisioned nothing
                        // even once the property said it could — the set_state path is
                        // the ONLY login an e2e takes.
                        if (HydrationSessionEnabled
                            && _rpcClient is NestRpcClient hydrationRpc)
                        {
                            // The loop's bearer read is synchronous by contract and
                            // this seam has no bearer-caching machine behind it (the
                            // set_state path never enters StartMainAppAsync), so the
                            // acquire is memoized behind the first tick's read. It
                            // blocks the convergence loop's own thread once — never
                            // the UI thread, and strictly cheaper than the delegate
                            // it replaces, which awaited the same call EVERY tick.
                            //
                            // Memoize SUCCESS ONLY. A `Lazy<string?>` whose factory
                            // catches and returns null caches that null FOREVER, so one
                            // transient RPC blip at login — precisely when this fires,
                            // and observed as `content-key resolve failed … rpc
                            // disconnected` — permanently starves every convergence
                            // tick, the agent is never provisioned, and the failure
                            // surfaces much later as "the sync engine never started".
                            // The production path never had this trap: it reads
                            // `machine.CurrentBearer()` fresh on each tick. Retrying
                            // until the first success keeps the original fix's acquire-once intent
                            // without making a blip terminal.
                            // ⚠ The acquire runs on ITS OWN task; the loop's read is a pure
                            // field read. That split is the contract, not a style choice:
                            // `FfiProvisioningBearerSource.CurrentBearer` is documented as "a
                            // cheap SYNCHRONOUS read of the bearer the app currently holds",
                            // and UniFFI invokes it on the convergence loop's own tokio worker
                            // thread. Blocking that thread on `GetBearerTokenAsync()` — which
                            // itself completes on the shared Rust runtime — is sync-over-async
                            // on the very runtime being awaited, so the first tick can park
                            // forever: no `RefreshBearer`, no `ProvisionCapability`, no engine,
                            // and no agent-reachable edge either, all of it SILENT because
                            // `fauna_ipc::convergence` logs nothing. Production never had this
                            // shape — it reads the launch machine's already-cached token — and
                            // neither did the pre-retirement e2e seam, whose bearer source was
                            // `async` and therefore awaited rather than blocked.
                            // Retry-until-first-success, so a transient blip at login (the
                            // `content-key resolve failed … rpc disconnected` window) is not
                            // terminal; a `Lazy<string?>` would cache that null forever.
                            // The expiry rides with the token (one immutable tuple, published
                            // by one reference write) so the agent is handed the deadline of
                            // exactly the bearer it got.
                            Tuple<string, ulong>? cachedBearer = null;
                            _ = Task.Run(async () =>
                            {
                                while (Volatile.Read(ref cachedBearer) is null)
                                {
                                    try
                                    {
                                        var (token, expiresAt) = await client.GetBearerWithExpiryAsync().ConfigureAwait(false);
                                        if (!string.IsNullOrEmpty(token))
                                        {
                                            Volatile.Write(ref cachedBearer, Tuple.Create(token, expiresAt));
                                            // Wake the loop rather than let it wait out a whole
                                            // tick interval on the token it was starved for.
                                            _syncAgent.Current?.Poke();
                                            return;
                                        }
                                    }
                                    catch { /* transient — retry below */ }
                                    await Task.Delay(TimeSpan.FromSeconds(2)).ConfigureAwait(false);
                                }
                            });
                            StartHydrationSession(
                                bearerSource: () => Volatile.Read(ref cachedBearer) is { } b ? (b.Item1, b.Item2) : null,
                                rpc: hydrationRpc,
                                deviceId: account.ResolveDeviceId() ?? "");
                        }
                    }
                }
            }
        }

        if (command.TryGetValue("nav", out var navObj) && navObj is System.Text.Json.JsonElement navEl)
        {
            if (navEl.TryGetProperty("stack", out var stackEl)
                && stackEl.ValueKind == System.Text.Json.JsonValueKind.Array
                && stackEl.GetArrayLength() > 0)
            {
                var first = stackEl[0];
                if (first.TryGetProperty("view", out var viewEl) && viewEl.ValueKind == System.Text.Json.JsonValueKind.String)
                {
                    var view = viewEl.GetString()!;
                    // Shell sub-pages: the shared action layer pushes a nested stack
                    // [{view:"admin"}, {view:"admin", id:"<sub-page>"}] (or the same
                    // for settings — the linux GTK Stack child name). Windows enters
                    // the distinct shell (AdminShellPage / SettingsShellPage,
                    // admin.md / settings.md § Navigation model) and routes the
                    // deepest entry's id to a sub-page within it; a bare "admin"
                    // lands on the dashboard, a bare "settings" on the Status
                    // sub-page.
                    string? subId = null;
                    var last = stackEl[stackEl.GetArrayLength() - 1];
                    if (last.TryGetProperty("id", out var idEl) && idEl.ValueKind == System.Text.Json.JsonValueKind.String)
                    {
                        subId = idEl.GetString();
                    }
                    // {view:"profile", actor_id:<hex>} opens ANOTHER actor's profile
                    // (OTHER — the subscriber-browse offers + follow button); absent →
                    // the viewer's own (SELF). profile.md § Layout & flow. An actor_id
                    // naming the VIEWER must also normalize back to SELF (the shared
                    // fauna_core::format::profile_nav_target door, over the same
                    // trim + ASCII-case-insensitive self-compare linux/tui/android/apple
                    // already share — priority #1/#2).
                    string? profileActor = null;
                    string? profileHome = null;
                    if (last.TryGetProperty("actor_id", out var actorEl) && actorEl.ValueKind == System.Text.Json.JsonValueKind.String)
                    {
                        var selfActorId = _cryptoService?.HasKey == true ? _cryptoService.ActorIdHex : null;
                        profileActor = uniffi.fauna_ffi.FaunaFfiMethods.ProfileNavTarget(actorEl.GetString() ?? "", selfActorId);
                        // The entry's optional home_nest_url is the open target's routing
                        // hint (testing.md convention 11), passed into the same opener.
                        if (last.TryGetProperty("home_nest_url", out var homeEl) && homeEl.ValueKind == System.Text.Json.JsonValueKind.String
                            && !string.IsNullOrWhiteSpace(homeEl.GetString()))
                            profileHome = homeEl.GetString();
                    }
                    _testCurrentView = view;
                    _currentErrorMessage = null;
                    // A nav ends an injected message's lifetime (tui's `apply_nav`),
                    // which is the ONLY page-driven clear it gets — see the field.
                    _injectedErrorMessage = null;
                    _currentWarningMessage = null;
                    _currentInfoMessage = null;
                    // .Then, NOT an overwrite: when this rides the same set_state as a
                    // `session` block (every login call site does — conftest's
                    // _login_app_as, admin_app, login_as_nest_admin, …), that block has
                    // already queued the MainPage hand-off carrying the FRESH clients it
                    // just built. Overwriting it dropped that hand-off, so on a second
                    // login with no intervening reset() the guard below saw MainPage
                    // already in the frame, skipped the re-navigation, and left MainPage
                    // bound to the clients DisposeNestClients had just disposed
                    // (ObjectDisposedException on the next call). It also dropped the
                    // stale-dialog dismissal that block performs.
                    navAction = navAction.Then(async () =>
                    {
                        if (view == "welcome")
                        {
                            var navParam = new ServiceClients(
                                _nestClient ?? new DirectNestClient(closuredNestUrl ?? $"https://127.0.0.1:{DefaultNestPort}", _cryptoService!),
                                _cryptoService!, account);
                            rootFrame.Navigate(typeof(Views.OnboardingPage), navParam);
                        }
                        else
                        {
                            if (rootFrame.Content is not Views.MainPage)
                            {
                                if (_nestClient != null && _cryptoService != null)
                                {
                                    // Carry the login-built session (bounded await) so the
                                    // _convSession-gated folder gestures work under e2e.
                                    var conv = await AwaitE2eConvSessionAsync();
                                    var navParam = new ServiceClients(_nestClient, _cryptoService, account, conv, Rpc: _rpcClient);
                                    rootFrame.Navigate(typeof(Views.MainPage), navParam);
                                }
                            }
                            if (view == "admin")
                            {
                                // row 197 diagnostic: confirms whether MainPage.Current is
                                // null at this exact point (the `?.` would otherwise silently
                                // swallow the whole admin nav with no trace at all).
                                if (FaunaApp.Core.Logs.E2eTrace.Enabled)
                                {
                                    FaunaApp.Core.Logs.E2eTrace.Write(
                                        $"[nav] admin dispatch: MainPage.Current={(Views.MainPage.Current is null ? "NULL" : "set")}, " +
                                        $"rootFrame.Content={rootFrame.Content?.GetType().Name ?? "null"}");
                                }
                                Views.MainPage.Current?.NavigateToAdminSubPage(subId);
                            }
                            else if (view == "settings")
                            {
                                Views.MainPage.Current?.NavigateToSettingsSubPage(subId);
                            }
                            else if (view == "status")
                            {
                                // Status folds into the Settings shell's Status
                                // sub-page (mirrors linux: a saved/explicit "status"
                                // view selects the Settings shell). Satisfies the
                                // {"view":"status"} state-protocol nav from
                                // conftest.py / test_settings.py.
                                Views.MainPage.Current?.NavigateToSettingsSubPage("status");
                            }
                            else
                            {
                                // Stash the OTHER-profile target (null for SELF / any
                                // non-profile view) for ProfilePage.OnNavigatedTo to read.
                                PendingProfileTarget = view == "profile" ? profileActor : null;
                                PendingProfileHome = view == "profile" && profileActor is not null ? profileHome : null;
                                Views.MainPage.Current?.NavigateToView(view);
                            }
                        }
                    });
                }
            }
        }

        // Handle settings injection (inbox_mode)
        if (command.TryGetValue("settings", out var setObj) && setObj is System.Text.Json.JsonElement setEl)
        {
            if (setEl.TryGetProperty("inbox_mode", out var modeEl) && modeEl.ValueKind == System.Text.Json.JsonValueKind.String)
            {
                _testInboxMode = modeEl.GetString() ?? "";
            }
        }

        // e2e-only: deterministically widen FoldersPage's async load window so the
        // nav-readiness regression test (test_folder_wizard_nav_readiness.py) can prove
        // the wizard opens on an immediate post-navigate add-click even under a slow
        // load. One-shot — consumed by the next Page_Loaded. See Views.IAsyncLoadedPage.
        if (command.TryGetValue("folder_load_delay_ms", out var delayObj)
            && delayObj is System.Text.Json.JsonElement delayEl
            && delayEl.ValueKind == System.Text.Json.JsonValueKind.Number
            && delayEl.TryGetInt32(out var delayMs))
        {
            Views.FoldersPage.TestLoadDelayMs = delayMs;
        }

        // e2e-only: deterministically widen EmailFilterPanel's async load window so a
        // nav-readiness regression test can prove a filter row's edit affordance is
        // correct on an immediate post-navigate read even under a slow load. One-shot —
        // consumed by the next Panel_Loaded. See SettingsPrivacyPage.LoadComplete.
        if (command.TryGetValue("email_filter_load_delay_ms", out var filterDelayObj)
            && filterDelayObj is System.Text.Json.JsonElement filterDelayEl
            && filterDelayEl.ValueKind == System.Text.Json.JsonValueKind.Number
            && filterDelayEl.TryGetInt32(out var filterDelayMs))
        {
            Controls.EmailFilterPanel.TestLoadDelayMs = filterDelayMs;
        }

        // Handle messages injection (for E2E tests)
        if (command.TryGetValue("messages", out var msgObj) && msgObj is System.Text.Json.JsonElement msgEl)
        {
            if (msgEl.TryGetProperty("error", out var errEl))
            {
                // The injected-message slot, never the page mirror — see
                // `_injectedErrorMessage` for what rewrote the mirror under it.
                _injectedErrorMessage = errEl.ValueKind == System.Text.Json.JsonValueKind.String
                    ? errEl.GetString()
                    : null;
                // A displayed error is logged as `ViewModelBase.SetError` logs it,
                // so the displayed-banner→ring witness checks the same funnel.
                if (!string.IsNullOrEmpty(_injectedErrorMessage))
                    ShellLog.Error("App", _injectedErrorMessage);
            }
            if (msgEl.TryGetProperty("warning", out var warnEl))
            {
                _currentWarningMessage = warnEl.ValueKind == System.Text.Json.JsonValueKind.String
                    ? warnEl.GetString()
                    : null;
            }
            if (msgEl.TryGetProperty("info", out var infoEl))
            {
                _currentInfoMessage = infoEl.ValueKind == System.Text.Json.JsonValueKind.String
                    ? infoEl.GetString()
                    : null;
            }
            // Open/close InfoBars on the UI thread so FlaUI can see them. The error
            // shown is the same precedence SerializeState publishes (bar the agent
            // slot), so the UI half and the state half of the injection agree.
            var err = _injectedErrorMessage ?? _currentErrorMessage;
            var warn = _currentWarningMessage;
            var inf = _currentInfoMessage;
            navAction = navAction.Then(() =>
            {
                Views.MainPage.Current?.UpdateTestMessages(err, warn, inf);
                return Task.CompletedTask;
            });
        }

        if (command.TryGetValue("compose", out var composeObj) && composeObj is System.Text.Json.JsonElement composeEl)
        {
            if (composeEl.TryGetProperty("file", out var fileEl) && fileEl.ValueKind == System.Text.Json.JsonValueKind.String)
            {
                var filePath = fileEl.GetString();
                var target = composeEl.TryGetProperty("target", out var targetEl)
                    && targetEl.ValueKind == System.Text.Json.JsonValueKind.String
                    ? targetEl.GetString()
                    : null;
                if (filePath is not null && System.IO.File.Exists(filePath))
                {
                    byte[] bytes;
                    try { bytes = System.IO.File.ReadAllBytes(filePath); }
                    catch { return navAction; }

                    var mediaType = FaunaApp.Core.Helpers.MimeDetect.FromExtension(filePath);
                    // Strip EXIF for images, mirroring DmComposeBar.AttachFile_Click's
                    // real picker-completion path (conversations.md § Attachments).
                    if (target == "attachment-button" && mediaType.StartsWith("image/", StringComparison.OrdinalIgnoreCase))
                    {
                        bytes = FaunaApp.Core.Helpers.ExifStripper.Strip(bytes);
                    }
                    var fileName = System.IO.Path.GetFileName(filePath);
                    navAction = navAction.Then(() =>
                    {
                        // conversations' attachment-button routes to whichever composer
                        // is active; profile-edit-avatar/-banner route to the Profile
                        // page's edit form; everything else (feed's compose-file, or no
                        // target) preserves the historical feed-only default.
                        if (target == "attachment-button")
                        {
                            Views.ConversationsPage.Current?.StageAttachment(bytes, mediaType, fileName);
                        }
                        else if (target == "profile-edit-avatar" || target == "profile-edit-banner")
                        {
                            Views.ProfilePage.Current?.StageImageFromTestAgent(target, bytes, filePath);
                        }
                        else
                        {
                            var feed = Views.MainPage.Current?.ActiveFeedPage;
                            // No mediaType: the feed composer seals at submit and takes the
                            // MediaItem's MIME from the seal's own reply, never from a guess
                            // made at pick time (media.md § Encryption at rest).
                            feed?.FeedComposeBar.SetAttachment(bytes, fileName);
                        }
                        return Task.CompletedTask;
                    });
                }
            }
        }

        return navAction;
    }
#endif
}

/// <summary>
/// Navigation parameter carrying the nest HTTP client, crypto service, and secret store.
/// Pages extract what they need from this record.
///
/// <para><see cref="SeedSecret"/> is set by the launch-flow when the long-term
/// store has a secret but no nest URL — i.e. the user previously force-quit
/// after confirming an identity but before completing nest_login. The
/// <see cref="Views.OnboardingPage"/> reads it from <c>OnNavigatedTo</c> and
/// pre-seeds the wizard via <c>OnboardingViewModel.SeedIdentity(seed)</c>.
/// Null on every other path.</para>
///
/// <para>Marked <c>internal</c> because <see cref="Launch"/> is the
/// fauna-launch-machine's <c>LaunchMachine</c>, which UniFFI emits as
/// <c>internal</c>; the record is only ever constructed/consumed within the
/// FaunaApp shell anyway.</para>
/// </summary>
internal record ServiceClients(
    INestHttpClient Nest,
    ICryptoService Crypto,
    /// <summary>
    /// The served account's identity material (secret, nest URL, device id,
    /// handle/domain/tier) — a read-only view resolved through the registry on
    /// every access, so a page reads the account as it stands now.
    /// </summary>
    ISessionAccount Account,
    /// <summary>
    /// The shared-Rust conversations session built at login
    /// (<c>NestRpcClient.BuildConversationsSessionAsync</c>), or null in E2E /
    /// when no session could be built. The conversations page renders off
    /// <c>ConvSession?.Manager()</c>, falling back to
    /// <c>ConversationsManagerHost.Instance</c> (mock backends) — there is no
    /// client-side MLS state (<c>conversations.md</c> § Architectural rules #2).
    /// </summary>
    ConversationsSession? ConvSession = null,
    string? SeedSecret = null,
    /// <summary>
    /// Pre-seeded pending-invite slot per the onboarding client target-state
    /// design (tracked internally). Set by
    /// the launch flow when the long-term store has identity + a saved
    /// pending-invite record (case 3 of 4). OnboardingPage feeds it to
    /// <c>OnboardingViewModel.SeedPendingInvite(...)</c> which forwards
    /// to the machine; the wizard lands at <c>InviteRequest</c> with
    /// the snapshot hydrated.
    /// </summary>
    PendingInviteRecord? SeedPendingInvite = null,
    /// <summary>
    /// Set when the launch flow's silent challenge reports the secret
    /// is not registered on the saved nest (target spec §App-launch
    /// routing, fallback "Challenge endpoint reports the secret is not
    /// registered on this nest"). After seeding the identity, the page
    /// calls <c>OnboardingMachine.NavigateToInviteRequestForKnownNest</c>
    /// with these values so the wizard lands directly on
    /// <c>invite_request</c> rather than forcing the user to re-type
    /// their handle on <c>handle_entry</c>. Null on every other launch
    /// path. Carries the (nest_url, handle) tuple — <c>handle</c> may
    /// be empty when no cached handle exists, in which case the wizard
    /// will surface an empty input on the page.
    /// </summary>
    (string NestUrl, string Handle)? NavigateToInviteRequestForKnownNest = null,
    /// <summary>
    /// Set when the launch flow's silent challenge reports the secret
    /// is not registered on the saved nest AND
    /// the <c>fauna.setup.status</c> WS-RPC kind reports <c>claimed: false</c>
    /// (target spec §App-launch routing — silent-challenge fallback
    /// table, unclaimed-nest row). After seeding the identity,
    /// <see cref="Views.OnboardingPage"/> calls
    /// <c>OnboardingViewModel.NavigateToClaimCodeForKnownNest</c> with
    /// these values so the wizard lands directly on <c>claim_code</c>
    /// rather than forcing the user back through <c>handle_entry</c>.
    /// Mutually exclusive with
    /// <see cref="NavigateToInviteRequestForKnownNest"/>; on any
    /// setup-status reachability failure the launch flow takes the
    /// safer default and navigates to <c>invite_request</c> instead.
    /// </summary>
    (string NestUrl, string Handle)? NavigateToClaimCodeForKnownNest = null,
    /// <summary>
    /// Set by the factory-reset affordance (<c>admin-factory-reset-button</c>)
    /// after <c>fauna.admin.factory_reset</c> returns the post-reset claim code.
    /// The box was wiped (not the client), so the identity stays valid; after
    /// seeding it, <see cref="Views.OnboardingPage"/> calls
    /// <c>OnboardingViewModel.NavigateToClaimCodeForKnownNestWithCode</c> to land
    /// the wizard on <c>claim_code</c> with the returned code <b>pre-filled</b>
    /// (the human never sees it). The admin then re-claims and re-picks the
    /// storage mode (the box is mode-unresolved post-reset). Mutually exclusive
    /// with <see cref="NavigateToClaimCodeForKnownNest"/>. Per
    /// <c>docs/goal/behavior/mail-bridge-lifecycle.md</c> § Factory reset.
    /// </summary>
    (string NestUrl, string Handle, string ClaimCode)? FactoryResetReonboard = null,
    /// <summary>
    /// Set by the launch flow when the persisted awaiting-manual-DNS slot
    /// (<c>ISessionAccount.AwaitingDns</c>) resolved the launch machine to
    /// <c>WizardAt(AwaitingManualDns)</c> — the user provisioned a nest, chose
    /// "Set up later" for DNS, and quit before the claim landed. After seeding
    /// the identity, <see cref="Views.OnboardingPage"/> calls
    /// <c>OnboardingViewModel.SeedAwaitingManualDns</c> with these values so
    /// the wizard lands on <c>Done</c> with <c>wizard_outcome() ==
    /// AwaitingManualDns</c> and renders the "Almost ready" surface — the same
    /// state the same-session <c>dns_post_instructions</c> exit produces. Per
    /// <c>docs/goal/behavior/onboarding.md</c> § "Almost ready" surface.
    /// </summary>
    (string NestUrl, string Handle, string DnsRecordsJson, string ClaimCode)? SeedAwaitingManualDns = null,
    /// <summary>
    /// Set by the <c>launch-recover-button</c> click on <c>LaunchRetryPage</c>
    /// (box-recovery.md § Recovery UI (step 4), surviving-device entry): the
    /// launch-time reachable-nest box-list read
    /// (<c>DeploymentSeedCustody.LoadRecoverableBoxesAsync</c>) already
    /// resolved ≥1 custodied box off the saved nest before the button was
    /// revealed. <see cref="Views.OnboardingPage"/> calls
    /// <c>OnboardingViewModel.SeedIdentityForRecovery</c> (a superset of
    /// <see cref="SeedSecret"/>'s plain <c>SeedIdentity</c> — same imported
    /// secret, but flips <c>recovery_intent</c> and lands the step on
    /// <c>NestRecovery</c> directly) then pushes the already-read box list so
    /// the page renders <c>recover-box-item</c> rows with no further wait.
    /// Mutually exclusive with <see cref="SeedSecret"/> (this carries its own
    /// secret hex). Mirrors linux's <c>on_recover</c> closure
    /// (<c>seed_identity_for_recovery</c> + <c>set_recovery_boxes</c>).
    /// </summary>
    (string SecretHex, string[] Boxes)? RecoverFromLaunch = null,
    /// <summary>
    /// Set by the launch flow when the silent challenge was refused because this
    /// identity was SUCCEEDED (<c>LaunchSnapshot.supersededSuccessor</c>,
    /// identity-succession.md § Propagation → *Own device fleet*). <see
    /// cref="Views.OnboardingPage"/> hands it to
    /// <c>OnboardingViewModel.RouteSupersededRefusalAsync</c>, which lands the wizard
    /// on the import flow with the claim-free reason and upgrades it once the chain
    /// verifies the successor. Carries the refused identity's registry session
    /// material (either may be null — the claim-free message is then final).
    /// Mutually exclusive with <see cref="SeedSecret"/> (the route seeds its own).
    /// </summary>
    (string ClaimedSuccessor, string? SecretHex, string? NestUrl)? SupersededRefusal = null,
    /// <summary>
    /// Invoked by <see cref="Views.OnboardingPage"/> once the wizard
    /// reaches <see cref="OnboardingStep.Done"/> and
    /// <c>CompleteOnboarding</c> has finished writing device_id /
    /// nest_url / cached handle to the long-term store. The launch
    /// flow assigns this so it can navigate away from the wizard, re-run
    /// the launch machine over the now-populated store, and dispatch to
    /// the main app. Null on every launch path that doesn't expect a Done
    /// transition (e.g. the seed-only / re-onboard cases).
    /// </summary>
    System.Action? OnOnboardingCompleted = null,
    /// <summary>
    /// Wired to <c>OnboardingViewModel.OnPendingInvitePersisted</c> — the
    /// windows twin of apple's <c>onPendingInvitePersisted</c> (<c>d42bb27654</c>).
    /// Fired after EVERY pending-invite slot write, first-run and append alike;
    /// only the handler <see cref="AddAccountHandler"/> installs actually adopts
    /// (it checks <c>_appendingAccount</c> itself). See that installation site's
    /// doc for why the switch cannot be the VM's own job. Null wherever the page
    /// isn't an append-mode entry — a harmless no-op there.
    /// </summary>
    System.Action<string>? OnPendingInvitePersisted = null,
    /// <summary>
    /// Wired to <c>OnboardingViewModel.OnAwaitingDnsPersisted</c> — the same
    /// adoption seam as <see cref="OnPendingInvitePersisted"/>, for the wizard's
    /// <c>AwaitingManualDns</c> exit. <see cref="AddAccountHandler"/>
    /// installs the SAME handler for both, so an append that parks at "Almost
    /// ready" switches to the identity <c>PersistAwaitingDns</c> just registered.
    /// Null wherever the page isn't an append-mode entry — a harmless no-op there.
    /// </summary>
    System.Action<string>? OnAwaitingDnsPersisted = null,
    /// <summary>
    /// The <c>LaunchMachine</c> that drove this launch. Set on
    /// <c>MainPage</c>-bound constructions so <see cref="DirectNestClient"/>
    /// can consult the machine for the bearer / 401-refresh (wired in Task 3
    /// of the fauna-launch-machine migration). Null on onboarding-bound
    /// constructions and on the e2e bridge-<c>session</c> bypass.
    /// </summary>
    uniffi.fauna_launch_machine.LaunchMachine? Launch = null,
    /// <summary>
    /// The WS-RPC request/reply façade (events today; more clusters as they
    /// migrate). Set on <c>MainPage</c>-bound constructions — the events view
    /// models route through it — and null on onboarding-bound ones.
    /// </summary>
    NestRpcClient? Rpc = null,
    /// <summary>
    /// Set by <c>SearchResultsPage</c> when a <c>SearchNav.Post</c> row is
    /// activated (search.md § User actions). <c>FeedPage</c>'s load hook opens
    /// the post_detail dialog for this id — via the loaded feed if already
    /// present, else the deep-link resolve (<c>ResolvePost</c>) — the same
    /// contract every other app's search-nav leg implements. Null on every
    /// ordinary Feed navigation.
    /// </summary>
    string? DeepLinkPostId = null,
    /// <summary>
    /// Set beside <see cref="DeepLinkPostId"/> by <c>ConversationsPage</c> when a room-search
    /// <c>RoomSearchNav.Post</c> hit is activated in a room homed on ANOTHER nest
    /// (<c>room-search.md</c> § The surface → <i>Opening a hit</i>): the room's recorded home.
    /// <c>FeedPage</c> then opens the post through <c>resolve_post_from(post_id, home)</c>
    /// instead of <c>resolve_post</c>. Null for every other deep link.
    /// </summary>
    string? DeepLinkPostHomeUrl = null,
    /// <summary>
    /// Set by <c>SearchResultsPage</c> when a <c>SearchNav.Mail</c> or
    /// <c>SearchNav.Draft</c> row is activated — <c>true</c> for either arm,
    /// distinguishing "no conversations deep link" from "a Draft whose own
    /// <see cref="DeepLinkThreadId"/> happens to be null" (the new-thread
    /// compose case). <c>ConversationsPage</c>'s load hook opens the thread
    /// named by <see cref="DeepLinkThreadId"/>, or starts a new conversation
    /// when that is null and <see cref="DeepLinkMessageId"/> is also null.
    /// </summary>
    bool DeepLinkOpenConversations = false,
    string? DeepLinkThreadId = null,
    /// <summary>
    /// Set alongside <see cref="DeepLinkThreadId"/> for a <c>SearchNav.Mail</c>
    /// row only (never <c>Draft</c>, which has no message to select — search.md
    /// § State &amp; data shape). <c>Mail</c>'s contract is "open the thread
    /// *and* select this message in it"
    /// (<c>ConversationsManager.select_thread_and_message</c>,
    /// conversations.md § The selected message) — never a plain thread-jump.
    /// </summary>
    string? DeepLinkMessageId = null,
    /// <summary>
    /// Set by <c>SearchResultsPage</c> when a <c>SearchNav.File</c> row is
    /// activated. <c>MediaPage</c>'s load hook resolves the pair through
    /// <c>MediaMachine.LocateFile</c> — the durable (folder, path_hash)
    /// identity, never the rendered row's name (content-index.md, the id-space
    /// trap this row's diff repeats) — and opens that item's detail sheet.
    /// </summary>
    (long FolderId, string PathHash)? DeepLinkFile = null,
    /// <summary>
    /// Set by <c>SearchResultsPage</c> when a <c>SearchNav.Contact</c> hit's
    /// server-side resolve comes back empty — the card was deleted (or its
    /// index row is stale) between being indexed and being activated
    /// (search.md § Implementation status today, the Contact bullet's DROPPED
    /// case). <c>ContactsPage</c>'s load hook opens the Address Book segment
    /// (repainting whatever book picker rows still exist) and surfaces the
    /// ratified <c>card_not_found</c> string on <c>error-message</c> — never a
    /// silent no-op, and never <c>CardDetailPage</c> with nothing to show.
    /// </summary>
    bool DeepLinkContactNotFound = false,
    /// <summary>
    /// Set by <c>MainPage.OpenModerationAppeal</c> when a <c>dm-message-withheld-badge</c>
    /// is tapped (takedown-and-block.md § Legal takedown → <i>App render</i>, the fourth
    /// surface): the matched queue row's <c>content_id</c>. <c>ModerationPage</c> hands it
    /// to <c>ModerationViewModel.RequestAppeal</c>, so the queue read it starts opens that
    /// row's own appeal. Null on every ordinary Moderation navigation.
    /// </summary>
    string? DeepLinkAppealContentId = null,
    /// <summary>
    /// What a sign-out's erase could not remove, while it still owes work
    /// (<c>account-scoping.md</c> § Erasure follows scope → <i>the residue
    /// surface</i>) — what the one sign-out routine
    /// (<c>SignOutResidueSurface.Finish</c>) left after a sign-out or start-over,
    /// or at a signed-out launch; <c>null</c> when nothing is owed. <see cref="Views.OnboardingPage"/>
    /// hands it once in <c>OnNavigatedTo</c> to
    /// <c>OnboardingViewModel.SignOutResidue</c>, which
    /// <c>IdentityChoiceView</c> paints as the <c>sign-out-residue</c> view.
    /// Null on every other navigation.
    /// </summary>
    Core.Services.ISignOutResidueSurface? SignOutResidue = null);
