using System.Linq;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using uniffi.fauna_ffi;
using uniffi.fauna_conversations;
using uniffi.fauna_feed;
using uniffi.fauna_devices_machine;
using uniffi.fauna_labeler_catalog_machine;
using uniffi.fauna_atproto_settings_machine;
using uniffi.fauna_media_machine;
using uniffi.fauna_folders_machine;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_client_connected_apps;
using uniffi.fauna_client_dns;
using uniffi.fauna_client_pair;
using uniffi.fauna_client_config;
using uniffi.fauna_log;
using uniffi.fauna_client_capabilities;

namespace FaunaApp.Core.Services;

/// <summary>
/// WS-RPC request/reply plane to the nest, over the per-actor WebSocket
/// (the <c>fauna.*.*</c> kinds), plus the server→client push pumps it drives
/// (reconnect re-hydrate + knock toasts + the generic <c>fauna.notification</c>
/// / <c>fauna.calendar.changed</c> / <c>fauna.protocol.resync_required</c> push
/// dispatch) over the shared <c>FfiNestClient</c>
/// subscriptions. The second nest transport surface alongside
/// <see cref="DirectNestClient"/> (HTTP) — the dead push-only WebSocketService
/// was removed when notifications moved onto WS-RPC push. Wraps the UniFFI
/// <c>FfiNestClient</c> — the native
/// WS-RPC requester shared with Apple/Android and called natively by Linux —
/// connecting lazily on first use; the inner client reconnects + refreshes its
/// own bearer internally.
/// <para>Carries the migrated <c>fauna.{account,feed,posts}.*</c> clusters (plus
/// the bridge-feeds subscribe) and the Events surface over the encrypted CalDAV
/// store (<c>FfiCaldavClient</c>); the remaining clusters keep hitting
/// <see cref="DirectNestClient"/> until they migrate (the windows <c>*-ws-rpc</c>
/// TODOs). <c>internal</c> because its surface returns UniFFI-<c>internal</c>
/// reply records.</para>
/// </summary>
internal sealed class NestRpcClient : INestRpcClient, IAsyncDisposable
{
    /// The nest's https base URL. The UniFFI client swaps it to <c>wss</c> and
    /// appends <c>/api/v1/ws/{actor}</c> itself (libs/fauna-client
    /// <c>ws_adapter::build_ws_url</c>), so no scheme conversion is done here.
    private readonly string _nestUrl;
    private readonly ICryptoService _crypto;
    private readonly SemaphoreSlim _connectGate = new(1, 1);
    private FfiNestClient? _nest;
    /// <summary>Set first thing in <see cref="DisposeAsync"/>, before anything
    /// awaits — the signal <see cref="ConnectedAsync"/> re-checks after its own
    /// <c>Connect()</c> await, because a dispose racing an in-flight connect
    /// sees <c>_nest</c> still null (it is written only once <c>Connect()</c>
    /// returns) and so skips disconnecting the client that is about to land
    /// anyway.</summary>
    private volatile bool _disposed;
    /// <summary>Cached, per-session <c>FfiCaldavClient</c> for
    /// <see cref="CaldavQueryEventsSeededAsync"/>'s backstop poll (events.md
    /// § Implementation status today) — the seam's per-instance sync-tokens
    /// Mutex would be silently discarded every poll tick through the ordinary
    /// per-call <c>nest.Caldav()</c> accessor every other Caldav* method uses
    /// (`FfiNestClient::caldav` mints a fresh, stateless client each call, correct
    /// for those). Mirrors apple's identical finding (`APIClient.swift`'s
    /// `cachedSeededCaldavClient`). Safe across account switches: a NEW
    /// <see cref="NestRpcClient"/> is constructed on each one (`App.xaml.cs`), so
    /// a fresh session already gets a fresh cache with no manual teardown owed.</summary>
    private FfiCaldavClient? _cachedSeededCaldavClient;

    // ── reconnect re-hydrate ────────────────────────────────────────────
    /// <inheritdoc />
    public event Action? Reconnected;
    private Task? _reconnectPump;
    // The UI SynchronizationContext captured at StartReconnectPump (called on the
    // UI thread), so Reconnected is raised on the UI thread — its subscribers
    // (surface VMs) mutate XAML-bound state.
    private SynchronizationContext? _uiContext;
    private bool _uiContextIsAuthoritative;

    /// <summary>
    /// The ONE authoritative capture of the UI <see cref="SynchronizationContext"/>
    /// every <c>Raise…OnUi</c> marshals through. Call it exactly once, from the UI
    /// thread, at a defined point — today <c>MainPage.OnNavigatedTo</c>, which BOTH
    /// login paths (production's <c>StartMainAppAsync</c> and the TestAgent
    /// <c>set_state</c> login) reach on the UI thread.
    /// <para>
    /// Before this existed, the context was whatever the FIRST pump to start
    /// happened to see: three pumps captured with <c>??=</c> and
    /// <c>StartReconnectPump</c> with a bare <c>=</c>, so under the e2e login —
    /// where <c>StartPushPump</c> runs from the command body's THREAD-POOL thread
    /// and <c>StartReconnectPump</c> from the deferred UI-thread post-action — the
    /// right context won only by ordering ACCIDENT. Correct-by-accident is how a
    /// push ends up marshaled onto a non-UI thread and straight into bound WinUI
    /// state, the silent COMException trap .
    /// </para>
    /// Idempotent and last-writer-wins by design: a second call from the UI thread
    /// records the same context.
    /// </summary>
    public void CaptureUiContext()
    {
        _uiContext = SynchronizationContext.Current;
        _uiContextIsAuthoritative = true;
    }

    /// <summary>
    /// Best-effort capture for the window BEFORE <see cref="CaptureUiContext"/> has
    /// run — a pump may start while the shell is still being built, and marshaling
    /// through a merely-plausible context beats invoking the handler inline on the
    /// pump's own thread. Never overwrites the authoritative capture, and never
    /// overwrites an earlier fallback either: the authoritative call is what
    /// corrects a wrong guess, not the next pump to start.
    /// </summary>
    private void CaptureUiContextFallback()
    {
        if (!_uiContextIsAuthoritative) _uiContext ??= SynchronizationContext.Current;
    }

    /// The local sync agent, for the remote-change nudge relay (see
    /// <see cref="RunPushPumpAsync"/>). Owned here rather than wired from the app
    /// shell because the push pump is started from TWO places — the ordinary
    /// <c>StartMainAppAsync</c> path and the TestAgent <c>set_state</c> login — and
    /// a relay wired at only one of them is a gap that reproduces exactly as "sync
    /// is slow", which is how this one survived (see the arm's own comment).
    private readonly IAgentSyncNudge _syncAgent;

    /// Opens a registry view over the shell's ONE credential store, for the three
    /// recovery ceremonies that write to it (create/replace, the escrow re-seal,
    /// and the succession's successor-seed persist). Injected rather than reached
    /// for because <c>CredentialStore</c> lives in the app shell — Core must not
    /// know which backend the process chose. Null in unit tests and on the
    /// pre-identity paths; the three members that need it then refuse out loud
    /// rather than half-running a ceremony that writes a key nobody holds.
    private readonly Func<FfiAccountRegistry>? _accountRegistry;

    public NestRpcClient(
        string nestUrl, ICryptoService crypto, IAgentSyncNudge? syncAgent = null,
        Func<FfiAccountRegistry>? accountRegistry = null)
    {
        _nestUrl = nestUrl;
        _crypto = crypto;
        _accountRegistry = accountRegistry;
        // The default never nudges — it exists for unit tests that construct an RPC
        // client with no agent in play. EVERY production site must pass a real one; the
        // note above is why (an unwired relay reproduces silently as "sync is slow").
        _syncAgent = syncAgent ?? new AgentSyncNudge(() => null);
    }

    /// <inheritdoc />
    public string HomeUrl => _nestUrl;

    /// <summary>
    /// Start the reconnect pump (idempotent). Drives a loop over the shared
    /// <c>FfiNestClient.SubscribeReconnects</c> watch and raises
    /// <see cref="Reconnected"/> on every reconnect (a <c>Connected</c> after the
    /// first connect), marshaled onto the UI thread. Call once from
    /// <c>StartMainAppAsync</c> on the UI thread (the captured
    /// <see cref="SynchronizationContext"/> is what makes the marshal correct).
    /// Fire-and-forget, app lifetime — mirrors the App's TTL-refresh / auto-renew
    /// loops; the loop self-terminates when the watch sender drops (client
    /// disconnect). transport.md § Push events: observers re-pull on reconnect.
    /// </summary>
    public void StartReconnectPump()
    {
        if (_reconnectPump is not null) return;
        CaptureUiContextFallback();
        _reconnectPump = RunReconnectPumpAsync();
    }

    private async Task RunReconnectPumpAsync()
    {
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            var sub = nest.SubscribeReconnects();
            // next() resolves Some(counter) on each reconnect, None when the
            // client is torn down (watch sender dropped) → the loop ends.
            while (await sub.Next().ConfigureAwait(false) is not null)
            {
                RaiseReconnectedOnUi();
            }
        }
        catch (Exception ex)
        {
            // Best-effort, fire-and-forget: a pump fault must never surface into
            // the app. Reconnect re-hydrate degrades to manual refresh.
            ShellLog.Warn("NestRpcClient", $"reconnect pump exited: {ex.Message}");
        }
    }

    private void RaiseReconnectedOnUi()
    {
        var handler = Reconnected;
        if (handler is null) return;
        if (_uiContext is not null)
            _uiContext.Post(_ => handler(), null);
        else
            handler();
    }

    // ── connection-state indicator ──────────────────────────────────────
    /// <inheritdoc />
    public event Action<FfiConnectionState>? ConnectionStateChanged;
    private Task? _connStatePump;

    /// <summary>
    /// Start the connection-state pump (idempotent). Drives a loop over the shared
    /// <c>FfiNestClient.SubscribeConnectionState</c> watch and raises
    /// <see cref="ConnectionStateChanged"/> on every transition — the first
    /// resolving the <i>current</i> state — marshaled onto the captured UI
    /// <see cref="SynchronizationContext"/>. Feeds the global <c>connection-status</c>
    /// indicator (<c>MainViewModel</c>). Fire-and-forget, app lifetime; the loop
    /// self-terminates when the watch sender drops (client disconnect). Native twin
    /// of linux's top-of-sidebar indicator update off its <c>WsEvent</c> pump.
    /// transport.md § Connection-status indicator.
    /// </summary>
    public void StartConnectionStatePump()
    {
        if (_connStatePump is not null) return;
        CaptureUiContextFallback();
        _connStatePump = RunConnectionStatePumpAsync();
    }

    private async Task RunConnectionStatePumpAsync()
    {
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            var sub = nest.SubscribeConnectionState();
            // Next() resolves the current state first, then each transition; null
            // when the client is torn down (watch sender dropped) → the loop ends.
            while (await sub.Next().ConfigureAwait(false) is { } state)
            {
                RaiseConnectionStateOnUi(state);
                // A `Disconnected` is also where a supervisor that stopped for good
                // says why (the stop is recorded before the state is announced). A
                // session-ending verdict — a re-mint refused as superseded, a
                // refused sign-in, a changed nest identity — goes to App, which
                // routes it to the launch surface; the session ends here. apple's
                // connection-state observer and tui's/linux's pumps are the model.
                if (state == FfiConnectionState.Disconnected)
                {
                    var verdict = nest.SessionEndingVerdict();
                    ShellLog.Info("NestRpcClient",
                        $"[conn] disconnected; session-ending verdict: {verdict?.ToString() ?? "none"}");
                    if (verdict is { } ending)
                    {
                        RaiseSessionEndingOnUi(ending);
                        break;
                    }
                }
            }
        }
        catch (Exception ex)
        {
            // Best-effort, fire-and-forget: a pump fault must never surface into the
            // app. The indicator degrades to its last painted state.
            ShellLog.Warn("NestRpcClient", $"connection-state pump exited: {ex.Message}");
        }
    }

    /// <summary>
    /// Raised once, on the UI thread, when this client's supervisor stopped for good
    /// on a session-ending verdict (<c>FfiNestClient.SessionEndingVerdict</c>, read on
    /// the <c>Disconnected</c> the connection-state pump sees). App routes it through
    /// <see cref="SessionEndingRoute.EscalateAsync"/>. Fires only while the pump runs
    /// (<see cref="StartConnectionStatePump"/>). Concrete-only: the one subscriber is
    /// App, which holds this type.
    /// </summary>
    internal event Action<FfiSessionEndingVerdict>? SessionEnding;

    private void RaiseSessionEndingOnUi(FfiSessionEndingVerdict verdict)
    {
        var handler = SessionEnding;
        if (handler is null) return;
        if (_uiContext is not null)
            _uiContext.Post(_ => handler(verdict), null);
        else
            handler(verdict);
    }

    private void RaiseConnectionStateOnUi(FfiConnectionState state)
    {
        var handler = ConnectionStateChanged;
        if (handler is null) return;
        if (_uiContext is not null)
            _uiContext.Post(_ => handler(state), null);
        else
            handler(state);
    }

    // ── knock push → OS toast + roster refresh ──────────────────────────
    /// <inheritdoc />
    public event Action<FfiKnock>? KnockReceived;
    private Task? _knockPump;

    /// <summary>
    /// Start the knock pump (idempotent). Drives a loop over the shared
    /// <c>FfiNestClient.SubscribeKnocks</c> stream and raises
    /// <see cref="KnockReceived"/> with the decoded knock on every inbound
    /// <c>fauna.knock</c> push, marshaled onto the UI thread. Call once from
    /// <c>StartMainAppAsync</c> on the UI thread (the captured
    /// <see cref="SynchronizationContext"/> makes the marshal correct).
    /// Fire-and-forget, app lifetime; the loop self-terminates when the push source
    /// closes (client disconnect). Re-homes the Windows knock OS toast (App) + the
    /// contacts roster refresh (ContactsPage) onto WS-RPC push — the dead
    /// WebSocketService fed these. conversations.md § Where logic lives.
    /// </summary>
    public void StartKnockPump()
    {
        if (_knockPump is not null) return;
        CaptureUiContextFallback();
        _knockPump = RunKnockPumpAsync();
    }

    private async Task RunKnockPumpAsync()
    {
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            var sub = nest.SubscribeKnocks();
            // Next() resolves the decoded knock, null when the push source closes
            // (client torn down) → the loop ends.
            while (await sub.Next().ConfigureAwait(false) is { } knock)
            {
                RaiseKnockOnUi(knock);
            }
        }
        catch (Exception ex)
        {
            // Best-effort, fire-and-forget: a pump fault must never surface into
            // the app. Knock toasts + roster refresh degrade to manual refresh.
            ShellLog.Warn("NestRpcClient", $"knock pump exited: {ex.Message}");
        }
    }

    private void RaiseKnockOnUi(FfiKnock knock)
    {
        var handler = KnockReceived;
        if (handler is null) return;
        if (_uiContext is not null)
            _uiContext.Post(_ => handler(knock), null);
        else
            handler(knock);
    }

    // ── generic push pump (fauna.notification / calendar.changed / resync_required) ──
    /// <inheritdoc />
    public event Action<string, string>? CalendarPushChanged;
    /// <inheritdoc />
    public event Action<string, string>? AddressBookPushChanged;
    /// <inheritdoc />
    public event Action<string>? FolderChangedPushed;
    private Task? _pushPump;

    /// <summary>
    /// Start the push pump (idempotent). Drives a loop over the shared
    /// <c>FfiNestClient.SubscribePushes</c> stream — the one central dispatch
    /// over every inbound push kind (transport.md § Push events; the UniFFI
    /// twin of linux's <c>app.rs</c> <c>WsEvent::Push</c> match) — and
    /// dispatches per <see cref="FfiPushEvent"/> variant:
    /// <list type="bullet">
    /// <item><c>Notification</c> and <c>ResyncRequired</c> reuse the existing
    /// <see cref="Reconnected"/> re-hydrate fan-out — the notifications surface
    /// already re-fetches on it (<c>NotificationsViewModel</c>'s constructor),
    /// and a dropped-push resync sweep wants that same blanket re-pull, not a
    /// dedicated event.</item>
    /// <item><c>CalendarChanged</c> raises the new
    /// <see cref="CalendarPushChanged"/> event — <c>EventsPage</c> re-runs its
    /// ~10 s poll's refresh+rebuild path immediately. <c>EventsPage</c> ALSO
    /// subscribes to <see cref="Reconnected"/> now (the same handler), so an
    /// ordinary reconnect or a <c>ResyncRequired</c> sweep recovers it too —
    /// it used to be outside that sweep set entirely (transport.md § Which
    /// surfaces a push invalidates, the windows-leg audit).</item>
    /// <item><c>AddressBookChanged</c> raises <see cref="AddressBookPushChanged"/>
    /// (gated on the classifier's <c>addressBook</c> flag) — <c>ContactsPage</c>
    /// re-reads its books and the open book's cards while the Address Book segment
    /// shows, and ALSO on <see cref="Reconnected"/> (the reconnect re-pull the
    /// transport doctrine names as this push's correctness backstop).</item>
    /// <item><c>SyncChanged</c> nudges the resident sync agent's engine for that
    /// set to pull now, off its rescan cadence (file-sync.md § Remote-change
    /// nudge) — the <c>PullFolderNow</c> verb over the shared agent surface
    /// (<see cref="IAgentSyncNudge"/>), fire-and-forget (best-effort; a missed
    /// nudge costs only latency, the cadence is the backstop) — AND raises
    /// <see cref="FolderChangedPushed"/> so <c>FoldersPage</c> can live-refresh
    /// the currently-expanded set's device-activity roster, and
    /// <c>MediaPage</c> its cross-set aggregate. Both pages ALSO subscribe to
    /// <see cref="Reconnected"/> now, closing the same reconnect-sweep gap as
    /// Events.</item>
    /// <item><c>AccountUpdated</c> / <c>Other</c> are no-ops — no windows
    /// surface acts on them yet.</item>
    /// </list>
    /// Call once from <c>StartMainAppAsync</c> on the UI thread (the captured
    /// <see cref="SynchronizationContext"/> is what makes the marshal correct).
    /// Fire-and-forget, app lifetime; the loop self-terminates when the push
    /// source closes (client disconnect). Rides the SAME authenticated socket
    /// as the knock/reconnect pumps (<see cref="ConnectedAsync"/>) — no second
    /// WebSocket.
    /// </summary>
    public void StartPushPump()
    {
        if (_pushPump is not null) return;
        CaptureUiContextFallback();
        _pushPump = RunPushPumpAsync();
    }

    private async Task RunPushPumpAsync()
    {
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            var sub = nest.SubscribePushes();
            // Next() resolves the next decoded push, null when the push source
            // closes (client torn down) → the loop ends.
            while (await sub.Next().ConfigureAwait(false) is { } ev)
            {
                // Fire-and-forget: a best-effort IPC hop must never stall the pump
                // loop for other events, and every reaction's own contract already
                // swallows its failures. `_ =` says the un-awaited Task is deliberate.
                _ = DispatchPush(ev);
            }
        }
        catch (Exception ex)
        {
            // Best-effort, fire-and-forget: a pump fault must never surface into
            // the app. Push-driven refresh degrades to the reconnect re-hydrate
            // + each surface's own poll backstop.
            ShellLog.Warn("NestRpcClient", $"push pump exited: {ex.Message}");
        }
    }

    /// <summary>
    /// Route ONE decoded push to its windows reaction. Split out of
    /// <see cref="RunPushPumpAsync"/> so the routing table is testable without a
    /// socket: the loop above is un-fakeable (it needs a live
    /// <c>FfiNestClient</c>), and leaving the table inside it meant nothing pinned
    /// which kinds windows actually acts on — which is precisely how the
    /// <c>SyncChanged</c> arm came to be missing for months while every unit test
    /// stayed green.
    /// <para>
    /// Returns the background work the dispatch started, or a completed task when
    /// the reaction is synchronous — so a test can await the effect instead of
    /// polling for it (testing.md convention 14).
    /// </para>
    /// </summary>
    internal Task DispatchPush(FfiPushEvent ev)
    {
        // Whether-to-fire per surface is derived from the shared classifier
        // (transport.md § Which surfaces a push invalidates) rather than
        // re-hand-matching the mapping — mirrors android's `ApiClient.kt`
        // dispatch and apple's `startPushObserver`. The classifier answers
        // STALENESS only ("the seam answers staleness, never side effects");
        // the payload-carrying reactions below (which calendar, which folder)
        // and the sync-agent nudge stay a raw match on `ev`, since
        // `FfiStaleSurfaces`'s booleans carry no payload.
        var stale = FaunaFfiMethods.StaleSurfacesForPushEvent(ev);
        switch (ev)
        {
            case FfiPushEvent.Notification:
            case FfiPushEvent.ResyncRequired:
                RaiseReconnectedOnUi();
                return Task.CompletedTask;
            case FfiPushEvent.CalendarChanged calendarChanged:
                if (stale.events)
                    RaiseCalendarPushChangedOnUi(calendarChanged.actorId, calendarChanged.calendarId);
                return Task.CompletedTask;
            case FfiPushEvent.AddressBookChanged addressBookChanged:
                // The carddav twin of the calendar arm (transport.md § Push events,
                // `fauna.addressbook.changed`): a card or book write landed in one of
                // this actor's address books. Its own event, not the FFI variant name,
                // for the same reason as CalendarPushChanged.
                if (stale.addressBook)
                    RaiseAddressBookPushChangedOnUi(addressBookChanged.actorId, addressBookChanged.addressbookId);
                return Task.CompletedTask;
            case FfiPushEvent.SyncChanged syncChanged:
                // A record landed in a folder this device participates in
                // (file-sync.md § Remote-change nudge). The windows sync agent is
                // bearer-only — it holds no WS connection of its own, so this app is
                // the ONLY thing that hears the push, and relaying it is what makes a
                // second device's save appear in seconds. Without the relay the sole
                // remaining delivery path is the agent's 300 s rescan tick, which is
                // why this read as "windows sync is slow" rather than as a missing arm.
                //
                //
                // The UI raise (FoldersPage's live device-activity refresh) IS
                // marshaled — RaiseFolderChangedOnUi posts through _uiContext —
                // but the nudge itself stays deliberately unmarshaled: no XAML
                // state is touched there, and it should leave on the pump's own
                // thread rather than queue behind UI work. The nudge is a side
                // effect, not a staleness reaction, so it is unconditional —
                // only the UI raise is gated on the classifier.
                if (stale.media)
                    RaiseFolderChangedOnUi(syncChanged.folder);
                return NudgeSyncAgentAsync(syncChanged.folder, syncChanged.folderHash);
            case FfiPushEvent.AccountUpdated:
            case FfiPushEvent.Other:
                // Not modelled on windows yet — no surface acts on these.
                return Task.CompletedTask;
            default:
                return Task.CompletedTask;
        }
    }

    /// <summary>
    /// Relay a <c>SyncChanged</c> push to the local agent as <c>PullFolderNow</c> —
    /// the windows twin of linux's <c>sync_agent::pull_set_now</c> and tui's
    /// <c>SyncAgentState::pull_set_now</c>, all three landing on the one shared IPC
    /// verb (priority #3).
    /// <para>
    /// Fire-and-forget and fully swallowed: the agent may be absent (sync not set
    /// up on this device) or the set may have no resident engine, both of which are
    /// ordinary states, and the rescan tick remains the correctness backstop. A
    /// throw here would kill the push pump and take every other push kind with it.
    /// </para>
    /// </summary>
    private async Task NudgeSyncAgentAsync(string folder, byte[]? folderHash)
    {
        try
        {
            await _syncAgent.PullFolderNowAsync(folder, folderHash).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient",
                $"remote-change pull-now nudge failed for '{folder}' (rescan tick will catch up): {ex.Message}");
        }
    }

    private void RaiseCalendarPushChangedOnUi(string actorId, string calendarId)
    {
        var handler = CalendarPushChanged;
        if (handler is null) return;
        if (_uiContext is not null)
            _uiContext.Post(_ => handler(actorId, calendarId), null);
        else
            handler(actorId, calendarId);
    }

    private void RaiseAddressBookPushChangedOnUi(string actorId, string addressbookId)
    {
        var handler = AddressBookPushChanged;
        if (handler is null) return;
        if (_uiContext is not null)
            _uiContext.Post(_ => handler(actorId, addressbookId), null);
        else
            handler(actorId, addressbookId);
    }

    private void RaiseFolderChangedOnUi(string folder)
    {
        var handler = FolderChangedPushed;
        if (handler is null) return;
        if (_uiContext is not null)
            _uiContext.Post(_ => handler(folder), null);
        else
            handler(folder);
    }

    /// <summary>
    /// Ensure the WS-RPC connection is up, returning the connected client. The
    /// secret is read here (not at construction) so the façade can be built
    /// before the keypair is loaded; the WS handshake authenticates via a
    /// silent challenge, independent of the HTTP bearer.
    /// </summary>
    private async Task<FfiNestClient> ConnectedAsync()
    {
        if (_nest is not null) return _nest;
        Logs.E2eTrace.Write("[connect] cache miss, awaiting gate");
        await _connectGate.WaitAsync().ConfigureAwait(false);
        Logs.E2eTrace.Write("[connect] gate acquired");
        try
        {
            if (_nest is null)
            {
                Logs.E2eTrace.Write("[connect] constructing FfiNestClient");
                var nest = new FfiNestClient(_nestUrl, _crypto.SecretBytes);
                Logs.E2eTrace.Write("[connect] constructed; calling Connect()");
                await nest.Connect().ConfigureAwait(false);
                Logs.E2eTrace.Write("[connect] Connect() returned");
                if (_disposed)
                {
                    // DisposeAsync ran while Connect() was in flight: it saw
                    // _nest still null and so never disconnected anything —
                    // this freshly connected client is the only handle on the
                    // reconnect supervisor DisposeAsync meant to stop. Tear it
                    // down here instead of caching it onto an already
                    // torn-down wrapper.
                    Logs.E2eTrace.Write("[connect] disposed mid-connect; disconnecting the client it minted");
                    try { await nest.Disconnect().ConfigureAwait(false); }
                    catch { /* best-effort: the wrapper is being discarded regardless */ }
                    nest.Dispose();
                    throw new ObjectDisposedException(nameof(NestRpcClient));
                }
                _nest = nest;
            }
            return _nest;
        }
        finally
        {
            // DisposeAsync disposes the gate without waiting for a holder to
            // release it, so a dispose racing this call can make Release()
            // throw ObjectDisposedException — tearing down is not this call's
            // problem to report.
            try { _connectGate.Release(); }
            catch (ObjectDisposedException) { /* disposed mid-flight; nothing to release into */ }
        }
    }

    // ── fauna.conversations.keypackage.* ────────────────────────────────

    /// <summary><c>fauna.conversations.keypackage.count</c> — remaining
    /// non-expired key packages for the connection actor.</summary>
    public async Task<int> KeypackageCountAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var reply = await nest.Conversations().KeypackageCount(_crypto.ActorIdHex).ConfigureAwait(false);
        return (int)reply.count;
    }

    /// <summary><c>fauna.conversations.keypackage.upload</c> — publish raw MLS
    /// KeyPackage bytes; returns how many were stored.</summary>
    public async Task<uint> KeypackageUploadAsync(IReadOnlyList<byte[]> packages, bool lastResort = false)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var reply = await nest.Conversations().KeypackageUpload(packages.ToArray(), lastResort).ConfigureAwait(false);
        return (uint)reply.stored;
    }

    // ── fauna.conversations.* session (shared ConversationsSession) ─────────

    /// <inheritdoc />
    public async Task<ConversationsSession?> BuildConversationsSessionAsync(
        ConversationsManager manager, string? identityDomain = null, string? deviceIdHex = null)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);

        // self_address = "<handle>@<domain>" (conversations.md § State & data
        // shape → Self-address: live, never baked). The handle comes from the
        // authenticated fauna.account.get (empty-tolerated by the backend). The
        // domain is the CALLER-resolved identity domain — production passes
        // LaunchSnapshot.identity.domain (the handle's own domain as the nest
        // reports it, which need not equal the nest hostname); the e2e
        // set_state login path has no LaunchMachine at all, so it resolves the
        // domain through the same fauna.actor.by_handle door the recipient
        // picker resolves PEERS through (mirrors apple's FaunaKit
        // APIClient.e2eSelfAddress) — both sides of peer_domain_for's
        // same-nest comparison are then answered by one nest call and cannot
        // disagree by construction. Only compose when BOTH halves are
        // non-empty — an empty local part or empty domain is the forbidden
        // "@nest.example" shape; a transient identity outage or an unresolved
        // domain drops to the empty string, never a half-composed address.
        var identity = await GetIdentityAsync().ConfigureAwait(false);
        var handle = identity?.Handle ?? "";
        string domain;
        if (identityDomain is not null)
        {
            domain = identityDomain;
        }
        else if (handle.Length > 0)
        {
            // A handle that already carries a qualifier keeps it — pass the
            // local part and let the nest echo the domain it was asked
            // about, the same multi-domain rule ResolveRecipient follows.
            var atIndex = handle.IndexOf('@');
            var local = atIndex >= 0 ? handle[..atIndex] : handle;
            string? typed = atIndex >= 0 ? handle[(atIndex + 1)..] : null;
            handle = local;
            try
            {
                var resolved = await FaunaFfiMethods.ResolveHandle(_nestUrl, local, typed).ConfigureAwait(false);
                domain = resolved.Length == 3 ? resolved[2] : "";
            }
            catch
            {
                domain = "";
            }
        }
        else
        {
            domain = "";
        }
        var selfAddress = handle.Length > 0 && domain.Length > 0 ? $"{handle}@{domain}" : "";

        // Account-scoped MLS store: <base>\<actor>\mls.db, derived from THIS
        // session's own secret (the actor whose MLS store this is, is by
        // construction the actor whose session is being built) — never a
        // process-wide "active account" global. account-scoping.md § Serialized
        // switching.
        var mlsDbPath = AccountStateDir.MlsDbPath(_crypto.HasKey ? _crypto.ActorIdHex : null);
        System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(mlsDbPath)!);

        // Build OVER the caller's manager (never a fresh internal one) so the real
        // rails register onto the SAME instance the app already holds — see the
        // interface doc for why (the manager-swap defect this closes).
        // Seat this desktop at the advisory `index` lease (participants.md
        // § Coordination primitive → *The `index` kind under the lease*), so the
        // Task-delegation row names this box as builder-of-record instead of reading
        // Waiting-while-running. The id is app-owned state the FFI factory cannot
        // derive — the caller passes `ISessionAccount.DeviceId`, the same
        // plumbing `TaskDelegationListAsync` uses. Null (a caller with no id yet)
        // is a supported wiring, not a stub: the builder still indexes everything,
        // it just does not coordinate. Shared Rust gates the seat on the same
        // build-vs-query constant the builder is gated on, so this cannot seat a
        // client that does not build.
        // Retired owner keys after an identity succession (sync-agent.md
        // § Credential model), feeding the `__mls` post-succession re-seal —
        // resolved off the same injected registry the recovery ceremonies
        // above use, empty in unit tests (`_accountRegistry` null) or when the
        // identity never succeeded.
        byte[][] predecessorBackupKeys;
        try
        {
            predecessorBackupKeys = _accountRegistry?.Invoke().PredecessorBackupKeys(_crypto.ActorIdHex)
                ?? Array.Empty<byte[]>();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient", $"[conversations] predecessor-backup-keys resolve failed (empty fallback): {ex.Message}");
            predecessorBackupKeys = Array.Empty<byte[]>();
        }

        return nest.ConversationsSessionOverManager(
            manager, selfAddress, _crypto.SecretBytes, mlsDbPath, IndexLeaseDevice(deviceIdHex),
            predecessorBackupKeys);
    }

    /// <summary>
    /// This device's `index`-lease seat, as the FFI factory wants it: the raw 32
    /// device-id bytes, or <c>null</c> when there is no usable id.
    ///
    /// <para>A helper rather than an inline <c>Convert.FromHexString</c> for the
    /// reason macOS's twin records (<c>FaunaKit/Core/APIClient.swift</c>
    /// <c>indexLeaseDevice</c>): <b>a malformed id must cost coordination, never
    /// the session.</b> Shared Rust *fails the call* on a wrong-length id —
    /// deliberately, so a real app bug cannot hide as a silently-unseated builder
    /// — which means an unexpected stored value (empty, truncated, a non-hex e2e
    /// patch) would otherwise take the whole conversations rail down with it: no
    /// chat, no mail receive. Screening here keeps the failure proportional and
    /// matches tui/linux, where no device id simply yields no seat.</para>
    /// </summary>
    private static byte[]? IndexLeaseDevice(string? deviceIdHex)
    {
        // 64 hex chars == the 32 bytes shared Rust requires. Checked before the
        // parse so a short/long id is screened without relying on the throw.
        if (deviceIdHex is null || deviceIdHex.Length != 64) return null;
        try
        {
            return Convert.FromHexString(deviceIdHex);
        }
        catch (FormatException)
        {
            return null;
        }
    }

    // ── per-user sync agent (shared SyncAgentProvisioner) ───────────────────

    /// <inheritdoc />
    public async Task<FfiSyncAgentProvisioner> BuildSyncAgentProvisionerAsync(
        byte[][] predecessorBackupKeys,
        byte[][] predecessorActorIds,
        string deviceId,
        string deviceLabel,
        FfiAgentSpawner spawner,
        FfiProvisioningBearerSource bearerSource,
        FfiAgentReachabilityObserver? reachabilityObserver)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var secret = _crypto.SecretBytes;
        return nest.SyncAgentProvisioner(
            secret,
            FaunaFfiMethods.BackupKeyDerive(secret),
            predecessorBackupKeys,
            predecessorActorIds,
            deviceId,
            deviceLabel,
            spawner,
            bearerSource,
            reachabilityObserver);
    }

    /// <inheritdoc />
    public async Task<IFfiPushRegistration> BuildPushRegistrationAsync(
        string intentPath, string actorId, string deviceId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return nest.PushRegistration(intentPath, actorId, deviceId);
    }

    /// <inheritdoc />
    public async Task<FfiReseedResult> ReseedCustodianStoreAsync(
        IFfiSyncAgentProvisioner agent, string thisDeviceId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await agent.ReseedCustodianStore(nest, _crypto.SecretBytes, thisDeviceId)
            .ConfigureAwait(false);
    }

    // ── W3 (account-data-plane.md § Workstreams) account-store runtime (account-data-plane.md § The account store) ──

    /// <summary>
    /// Host the W3 account-store runtime in this process (`account-data-plane.md`
    /// § The account store → *The client-side lifecycle*). Windows hosts no account store of its own — its preference
    /// surfaces ride the blob rail and its own scopes are walked by the
    /// co-located agent alone until this runs — and `apps/sync-agent.md` §
    /// Scope per platform lists a resident `fauna-sync-agent` for windows, so a
    /// healthy signed-in app may legitimately read `runtime: true, holder:
    /// false` once that agent wins the W5.1 engine-singleton election; hosting
    /// is still required of it, because the runtime is what the account's own
    /// preference surfaces read through.
    ///
    /// <para><b>Call independently of the conversations session, never folded
    /// into it</b> — `memberships: None` is a *supported* wiring (an app with
    /// no conversations rail must still host a runtime over its own-actor
    /// scopes), so the two calls need no ordering contract: the membership
    /// source re-reads the client's stashed session on every pump pass, so a
    /// runtime started first answers <i>cannot tell</i> until the session
    /// lands and then starts answering.</para>
    ///
    /// <para><b>The two inputs, and why each is that value</b> (mirrors
    /// apple's `FaunaClient.startAccountRuntime()` / android's
    /// `ApiClient.startAccountRuntime`):</para>
    /// <list type="bullet">
    /// <item><paramref name="appDataDir"/> — this app's own per-user data dir
    /// (<c>AccountStateDir.Base</c>, the same base <c>account_state_*</c>
    /// already takes). NOT the store root, which is a *sibling* under it that
    /// the shared assembly resolves itself.</item>
    /// <item><paramref name="deviceIdHex"/> — the SAME value the caller passes
    /// to <see cref="BuildConversationsSessionAsync"/>'s own
    /// <c>deviceIdHex</c>, screened through the same <see
    /// cref="IndexLeaseDevice"/> helper: a malformed id must cost
    /// coordination, never this call.</item>
    /// </list>
    ///
    /// <para><c>storeContainer</c> is always <c>null</c> here — it exists for
    /// sandboxed iOS; shared Rust states the desktop's own
    /// <c>NotApplicable</c> cloud-backup posture, matching macOS.</para>
    ///
    /// <para>This account's **attested** succeeded-from identities
    /// (<c>account-data-taxonomy.md</c> § The generation machinery → *The
    /// source of `prior`*, ruled 2026-09-13) are resolved by shared Rust off
    /// the injected <c>_accountRegistry</c> this hands it — the same registry
    /// the conversations-session leg above uses — for this session's own
    /// actor: their ids, and the key schedules a predecessor's preference rows
    /// are carried under. Nothing is attested in unit tests
    /// (<c>_accountRegistry</c> null) or when the identity never succeeded,
    /// which is fail-safe.</para>
    ///
    /// Best-effort like every other post-auth hook: a failure leaves every
    /// preference surface on the blob rail exactly as before this existed, and
    /// never fails a sign-in.
    /// </summary>
    public async Task StartAccountRuntimeAsync(string appDataDir, string? deviceIdHex)
    {
        try
        {
            // The registry itself: shared Rust resolves this session's attested
            // predecessors off it — the ids and the schedules their preference
            // rows are carried under. Null (unit tests) attests nothing.
            FfiAccountRegistry? accounts;
            try
            {
                accounts = _accountRegistry?.Invoke();
            }
            catch (Exception ex)
            {
                ShellLog.Warn("NestRpcClient", $"[account-runtime] account registry unavailable (no predecessors attested): {ex.Message}");
                accounts = null;
            }
            var nest = await ConnectedAsync().ConfigureAwait(false);
            await nest.StartAccountRuntime(appDataDir, storeContainer: null, IndexLeaseDevice(deviceIdHex), accounts)
                .ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient",
                $"[account-runtime] start failed; preference surfaces stay on the blob rail "
                + $"and this account's own scopes go unwalked: {ex.Message}");
        }
    }

    /// <summary>
    /// Stop this process's account runtime — sign-out / account-switch /
    /// factory-reset, never a plain quit (a quit ends the process, the handle
    /// drops, and the co-located agent takes the pump role over). Awaits the
    /// pump's in-flight pass, so the departing account has stopped writing by
    /// the time this returns.
    ///
    /// <para><b>Deliberately reads the cached client directly rather than
    /// <see cref="ConnectedAsync"/></b> — opening a fresh WS-RPC socket during
    /// a sign-out would authenticate as the identity being torn down. A
    /// client that never connected never installed a runtime through it
    /// either, so the no-op is correct rather than a gap (mirrors apple's
    /// <c>APIClient.stopAccountRuntime()</c>).</para>
    /// </summary>
    public async Task StopAccountRuntimeAsync()
    {
        if (_nest is null) return;
        try
        {
            await _nest.StopAccountRuntime().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient", $"[account-runtime] stop failed: {ex.Message}");
        }
    }

    /// <summary>
    /// The sign-out-shaped twin of <see cref="StopAccountRuntimeAsync"/> — call
    /// in its place wherever the credential erase that follows takes this
    /// machine's account-store slot (the writer key) with it: a plain account
    /// switch, or a reset/logout that leaves that slot in place, keeps
    /// <see cref="StopAccountRuntimeAsync"/> instead
    /// (<c>sync-agent-credentials.md</c> § Credential model → *The signed-out
    /// reconcile*). Superset of <see cref="StopAccountRuntimeAsync"/> — retires
    /// this machine's enrollment nest-side FIRST, while the runtime still holds
    /// the writer key, then runs the same local teardown (mirrors apple's
    /// <c>APIClient.stopAccountRuntimeForSignOut()</c>).
    /// </summary>
    public async Task StopAccountRuntimeForSignOutAsync()
    {
        if (_nest is null) return;
        try
        {
            await _nest.StopAccountRuntimeForSignOut().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient", $"[account-runtime] stop (sign-out) failed: {ex.Message}");
        }
    }

    /// <summary>
    /// Hand over this actor's conversations engine — closing <c>mls.db</c> and
    /// releasing <c>mls.db.lock</c> — before the shell erases the account-scoped
    /// directories. Idempotent; a no-op when no session was built.
    ///
    /// <para>The work is shared Rust
    /// (<c>FfiNestClient::release_account_scoped_stores</c>), so every UniFFI app
    /// gets the same release from the same seam; this wrapper exists only to give
    /// the caller something awaitable. <c>Task.Run</c> because the FFI call is
    /// synchronous and flushes the provider snapshot on its way out — short, but
    /// not something to run on the UI thread during a sign-out.</para>
    ///
    /// <para>⚠ Dropping C# handles is NOT a substitute and never was: the Rust
    /// client stashes the session and manager itself, so those stashes — not the
    /// shell's wrappers — are what keep the store open. See the Rust doc for the
    /// measurement.</para>
    /// </summary>
    public async Task ReleaseAccountScopedStoresAsync()
    {
        var nest = _nest;
        if (nest is null) return;
        try
        {
            await Task.Run(() => nest.ReleaseAccountScopedStores()).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            // Never throws into a sign-out: a failed release means the erase that
            // follows will report the still-open store itself (and now says so with
            // the path attached), which is a better place to see it than here.
            ShellLog.Warn(
                "NestRpcClient",
                $"[account-scope] releasing the conversations engine failed: {ex.Message}");
        }
    }

    /// <summary>
    /// The <c>account_pump_cycles</c> e2e state value as raw JSON, or
    /// <c>null</c> when this app has no connected client yet (pre-auth) — the
    /// serializer reports that as the key being absent. A plain atomic read of
    /// two counters plus two booleans (convention 11's corollary forbids
    /// blocking I/O on the state path) — reached through this accessor rather
    /// than a raw FFI handle for the same reason every other TestAgent read is.
    /// </summary>
    public string? AccountPumpCyclesJson() => _nest?.AccountPumpCyclesJson();

    /// <summary>
    /// The member-side succession report for <c>data.succession_witness</c>, as
    /// raw JSON — <c>null</c> until a conversations session has been built (no
    /// connected client yet, or the seat's identity secret would not parse),
    /// which the serializer reports as the key being absent rather than an
    /// empty report (<c>succession-propagation.md</c> § Propagation → *MLS
    /// groups*, the ✅ witness bullet). Rendered by
    /// <c>fauna_client_recovery::witness::state_json</c>, so this accessor
    /// republishes the FFI's own string rather than re-encoding anything — a
    /// field read, never a round trip (convention 11's corollary).
    /// </summary>
    public string? SuccessionWitnessStateJson() => _nest?.SuccessionWitnessStateJson();

    /// <summary>
    /// The <c>account_pump_now</c> poke — one full account-pump pass now,
    /// convention 14's <c>run_now</c> for the account plane. <c>false</c> when
    /// there is no connected client or no assembled runtime yet (pre-auth): a
    /// legitimate quiet no-op, honoured rather than dropped, with the
    /// consumer's own deadline poll on <c>account_pump_cycles</c> as the
    /// barrier that fails and names the app.
    /// </summary>
    public async Task<bool> AccountPumpNowAsync()
    {
        if (_nest is null) return false;
        try
        {
            return await _nest.AccountPumpNow().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient", $"[account-runtime] pump-now failed: {ex.Message}");
            return false;
        }
    }

#if DEBUG || FAUNA_E2E_AGENT
    /// <summary>
    /// The <c>device_set_state</c> e2e reader as raw JSON — whether
    /// <paramref name="deviceIdHex"/>'s <c>fauna.state.device-set</c> row reads
    /// Removed/Enrolled from this app's own account runtime
    /// (<c>account-data-taxonomy.md</c> § The generation machinery →
    /// <i>Fleet-scope reclamation</i>, clause (4)). <c>null</c> when this app has
    /// no connected client yet (pre-auth): never forces a connect, like
    /// <see cref="AccountPumpNowAsync"/>. The caller turns that into a
    /// <c>{"found":false}</c> report, because <c>null</c> is the wire signal for
    /// "reader unbuilt".
    /// Does NOT swallow a failure into <c>null</c>: the FFI export never throws
    /// (it answers a not-found report itself), so an exception is a transient the
    /// test agent must surface loudly, not something to read as "reader unbuilt".
    /// <para><b>Debug-only</b> (testing.md convention 15): the export is a
    /// <c>test-helpers</c> UniFFI seam the production <c>windows-ffi</c> flavor's
    /// bindings do not carry, so an ungated wrapper cannot compile in Release. Its
    /// only caller is the equally Debug-only <c>Testing.TestAgent</c>, via
    /// <c>App.DeviceSetStateForTest</c>.</para>
    /// </summary>
    public async Task<string?> DeviceSetStateJsonAsync(string deviceIdHex)
    {
        if (_nest is null) return null;
        return await _nest.DeviceSetStateJson(deviceIdHex).ConfigureAwait(false);
    }

    /// <summary>
    /// The <c>reconnect_backoff</c> e2e seam (<c>fauna_e2e_agent::RECONNECT_BACKOFF</c>
    /// owns the contract): pace this connection's reconnect retries —
    /// <c>{"initial_ms": N, "max_ms": M}</c> — or restore production pacing with
    /// <c>{}</c>. Only the PACE: the <c>Unreachable</c> threshold is never shortened
    /// (<c>transport-connection.md</c> § <c>Unreachable</c>). The payload goes to
    /// shared Rust verbatim, which parses and validates it.
    /// <para>Returns <c>false</c> with no client to pace (pre-auth) — never forces a
    /// connect, like <see cref="DeviceSetStateJsonAsync"/>. A malformed payload
    /// throws the FFI error, which the agent reports as a failed command.
    /// <b>Debug-only</b> for the same reason as <see cref="DeviceSetStateJsonAsync"/>:
    /// the export exists only in the <c>test-helpers</c> bindings.</para>
    /// </summary>
    public bool SetReconnectBackoffForTest(string payloadJson)
    {
        if (_nest is null) return false;
        _nest.SetReconnectBackoffForTest(payloadJson);
        return true;
    }
#endif

    /// <inheritdoc />
    /// <remarks>Reads the connected client's account runtime, so — like
    /// <see cref="AccountPumpNowAsync"/> — it never forces a connect: with no
    /// connected client there is no runtime and nothing to say (<c>null</c>).
    /// Unlike the pump poke it does NOT swallow a failure into <c>null</c>: the
    /// FFI export itself never throws (an unreadable slot is already <c>None</c>
    /// there), so an exception here is a transient the caller must be able to
    /// tell from "the refusal cleared". It propagates, and the page's load keeps
    /// the notice it painted.</remarks>
    public async Task<string?> AccountEnrollmentNoticeAsync()
    {
        if (_nest is null) return null;
        return await _nest.AccountEnrollmentNotice().ConfigureAwait(false);
    }

    /// <inheritdoc />
    /// <remarks>A free FFI function over the process's account runtime, so it
    /// needs no connected client: with no runtime assembled the shared rule
    /// answers the own id.</remarks>
    public Task<string?> ThisDeviceRowAsync(string? ownDeviceId)
        => uniffi.fauna_ffi.FaunaFfiMethods.DevicesThisDeviceRow(ownDeviceId);

    // ── fauna.drafts.* (shared DraftsSync — __drafts persistence v2) ─────────

    /// <summary>
    /// Build the stateful per-rail draft autosync over this connection (draft-
    /// persistence v2; docs/goal/behavior/file-sync.md § Drafts Sync) — the canonical
    /// shared <c>fauna_client_drafts::DraftsSync</c> wrapper (launch gate + last-saved
    /// baseline dedup), the same shape web/linux/android consume. The at-rest
    /// <c>BackupKey</c> seal key is derived inside the wrapper from this connection's
    /// authenticated identity (drafts are owner-only); the seal + WS call + gate live in
    /// shared Rust, never re-implemented in C#. Build once at login and hold it for the
    /// session — a fresh instance would re-close the gate and lose the dedup baseline.
    /// </summary>
    public async Task<IFfiDraftsSync> BuildDraftsSyncAsync(string rail)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return nest.DraftsSync(rail);
    }

    /// <summary>
    /// Build the events-rail draft autosync, typed rather than raw bytes
    /// (docs/goal/behavior/reserved-folders.md § Drafts Sync;
    /// docs/goal/ui/events.md § Persistence). Unlike <see cref="BuildDraftsSyncAsync"/>
    /// (the conversations/feed legs' generic bytes-in/bytes-out wrapper, hung off a
    /// shared manager that already owns the canonical encoding), the Events page has
    /// no manager on any app, so this face carries the five <c>event-form</c> fields
    /// across the boundary directly (<c>FfiNestClient.EventDrafts()</c> →
    /// <c>FfiEventDraftsSync</c>). Build once at login and hold it for the session —
    /// a fresh instance would re-close the launch gate and lose the last-saved
    /// baseline, exactly like <see cref="BuildDraftsSyncAsync"/>.
    /// </summary>
    public async Task<IFfiEventDraftsSync> BuildEventDraftsSyncAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return nest.EventDrafts();
    }

    // ── fauna.feed.* page (shared FeedManager) ──────────────────────────────

    /// <inheritdoc />
    public async Task<FfiFeedManager> BuildFeedManagerAsync()
    {
        // Reuse the shared, auto-reconnecting WS-RPC client (the same pattern as
        // BuildConversationsSessionAsync / the page machines), then hand the
        // manager the 32-byte actor secret so its submit_post can build + sign
        // posts (libs/fauna-ffi FfiNestClient::feed_manager → fauna_feed::FeedManager).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return nest.FeedManager(_crypto.SecretBytes);
    }

    // ── search page (shared SearchManager) ──────────────────────────────────

    /// <inheritdoc />
    public async Task<FfiSearchManager> BuildSearchManagerAsync()
    {
        // Reuse the shared, auto-reconnecting WS-RPC client (the same pattern as
        // BuildFeedManagerAsync) — no secret needed, searching signs nothing
        // (libs\fauna-ffi FfiNestClient::search_manager → fauna_client_search::SearchManager).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return nest.SearchManager();
    }

    /// <inheritdoc />
    public async Task<bool> AttachLocalSearchIndexAsync(FfiSearchManager manager)
    {
        // Same connected-client reuse as BuildSearchManagerAsync — the arm this
        // registers rides the SAME session's stashed index launcher
        // (libs\fauna-ffi FfiNestClient::attach_local_search_index).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.AttachLocalSearchIndex(manager).ConfigureAwait(false);
    }

    /// <summary>The SAME live manager the Feed page observes (<see cref="FeedManagerHost.Current"/>,
    /// the session's one instance), never a fresh
    /// <see cref="BuildFeedManagerAsync"/> instance — the manager caches
    /// this opt-in for its own signal producer, so a second instance would diverge from
    /// what the Feed page observes (mirrors <c>PersonalizationPage.DeleteCueRollupAsync</c>'s
    /// accessor). Reading the holder rather than <c>FeedViewModel.Current</c> directly keeps
    /// <c>Services</c> from depending on <c>ViewModels</c> (which already depends on
    /// <c>Services</c> via <c>FeedViewModel</c>'s <c>INestRpcClient</c> constructor parameter —
    /// see <see cref="FeedManagerHost"/>'s own doc comment). Null when the Feed page
    /// hasn't built the session's manager yet (in principle only — every shipped navigation
    /// path builds it first, since Feed is the default tab).</summary>
    private static FfiFeedManager SharedFeedManager() =>
        FeedManagerHost.Current
        ?? throw new InvalidOperationException(
            "No live FfiFeedManager — the Feed page has not built one yet.");

    /// <inheritdoc />
    public async Task<FfiReportShareStatus> SignalShareStatusAsync() =>
        await SharedFeedManager().SignalShareStatus().ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<FfiReportShareStatus> SetSignalSharingAsync(bool share) =>
        await SharedFeedManager().SetSignalSharing(share).ConfigureAwait(false);

    // ── encrypted CalDAV store (fauna.bridges.* via FfiCaldavClient) ─────
    // The Events surface (calendars + events + rsvp/reminder/invite) rides the
    // shared FfiCaldavClient against the encrypted CalDAV store the mail-bridge
    // MDA serves — the same store apple/android/web/linux read (events.md
    // § Encrypted store). All ids cross the FFI as lowercase hex strings.

    /// <summary><c>list_calendars</c> — the caller's CalDAV calendars.</summary>
    public async Task<IReadOnlyList<FfiCalendarRow>> CaldavListCalendarsAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Caldav().ListCalendars().ConfigureAwait(false);
    }

    /// <summary><c>create_calendar</c> — create a calendar named
    /// <paramref name="name"/>.</summary>
    public async Task CaldavCreateCalendarAsync(string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Caldav().CreateCalendar(name).ConfigureAwait(false);
    }

    /// <summary><c>query_events</c> — events in calendar
    /// <paramref name="calendarIdHex"/>.</summary>
    public async Task<IReadOnlyList<FfiCalEvent>> CaldavQueryEventsAsync(string calendarIdHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Caldav().QueryEvents(calendarIdHex).ConfigureAwait(false);
    }

    /// <summary><c>query_events_seeded</c> — the delta-sync backstop poll's
    /// cost-saving twin. Routes through <see cref="_cachedSeededCaldavClient"/>,
    /// never a fresh <c>nest.Caldav()</c> per call — see that field's own doc
    /// comment for why.</summary>
    public async Task<IReadOnlyList<FfiCalEvent>?> CaldavQueryEventsSeededAsync(string calendarIdHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var caldav = _cachedSeededCaldavClient ??= nest.Caldav();
        return await caldav.QueryEventsSeeded(calendarIdHex).ConfigureAwait(false);
    }

    /// <summary><c>query_invited_events</c> — events the caller is invited to.</summary>
    public async Task<IReadOnlyList<FfiCalEvent>> CaldavQueryInvitedEventsAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Caldav().QueryInvitedEvents().ConfigureAwait(false);
    }

    /// <summary><c>get_event</c> — full detail for event
    /// <paramref name="uidHashHex"/>, or <c>null</c> if absent.</summary>
    public async Task<FfiCalEvent?> CaldavGetEventAsync(string uidHashHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Caldav().GetEvent(uidHashHex).ConfigureAwait(false);
    }

    /// <summary><c>create_event</c> — create an event in calendar
    /// <paramref name="calendarIdHex"/>. Null <paramref name="location"/>/
    /// <paramref name="description"/> map to <c>""</c> (the FFI wants non-null).</summary>
    public async Task CaldavCreateEventAsync(
        string calendarIdHex, string summary, string dtstart, string dtend, string? location, string? description)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Caldav()
            .CreateEvent(calendarIdHex, summary, dtstart, dtend, location ?? "", description ?? "")
            .ConfigureAwait(false);
    }

    /// <summary><c>delete_event</c> — cancel event <paramref name="uidHashHex"/>.</summary>
    public async Task CaldavDeleteEventAsync(string uidHashHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Caldav().DeleteEvent(uidHashHex).ConfigureAwait(false);
    }

    /// <summary><c>rsvp_event</c> — RSVP to event <paramref name="uidHashHex"/>.
    /// <paramref name="response"/> is the typed <c>RsvpResponse</c> submission set
    /// (<c>going</c>/<c>interested</c>/<c>declined</c>). <c>tentative</c> is NOT
    /// representable here: it is an inbound-only state a stock CalDAV client sets,
    /// and the shared <c>apply_rsvp</c> refuses an unrecognized value instead of
    /// folding it to <c>NEEDS-ACTION</c> (caldav-server.md § RSVP semantics).</summary>
    public async Task CaldavRsvpEventAsync(string uidHashHex, uniffi.fauna_core.RsvpResponse response)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Caldav()
            .RsvpEvent(uidHashHex, response)
            .ConfigureAwait(false);
    }

    /// <summary><c>set_reminder</c> — set the caller's reminder
    /// <paramref name="offset"/> (<c>""</c> clears) for event
    /// <paramref name="uidHashHex"/>.</summary>
    public async Task CaldavSetReminderAsync(string uidHashHex, string offset)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Caldav().SetReminder(uidHashHex, offset).ConfigureAwait(false);
    }

    /// <summary><c>invite_attendee</c> — invite <paramref name="email"/> to event
    /// <paramref name="uidHashHex"/> (<c>""</c> re-sends to the existing roster).</summary>
    public async Task CaldavInviteAttendeeAsync(string uidHashHex, string email)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Caldav().InviteAttendee(uidHashHex, email).ConfigureAwait(false);
    }

    // ── fauna.posts.* ────────────────────────────────────────────────────
    // feed.list/posts/local.posts/create/delete + posts.create/get moved onto the
    // shared FfiFeedManager in the 2026-06 feed-snapshot lift (feed.md § State &
    // data shape); only posts.interact stays client glue (like/reply/repost).

    /// <summary><c>fauna.posts.get</c> → the post's extracted body text (the source of a
    /// moderation server-row spam train on the sealed client-write path). Mirrors linux
    /// <c>train_moderation_flow</c> (<c>posts_get</c> → <c>DecodePostFull().body</c>);
    /// tolerates a missing / undecodable post → <c>null</c> so the caller degrades to the
    /// server-side train.</summary>
    public async Task<string?> PostBodyTextAsync(string contentId)
    {
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            var bytes = await nest.Posts().PostsGet(contentId).ConfigureAwait(false);
            return FaunaFfiMethods.DecodePostFull(bytes).@body;
        }
        catch
        {
            return null;
        }
    }

    // ── fauna.account.* / fauna.quota.get ───────────────────────────────

    /// <summary><c>fauna.account.get</c> → the app's <see cref="IdentityInfo"/>
    /// (actor id + handle from the reply; nest URL is local config). Returns
    /// <c>null</c> when no keypair is loaded; on an unreachable nest falls back
    /// to local-only identity (actor id known from the keypair, null handle) so
    /// a transient outage doesn't bounce the user to onboarding — mirroring the
    /// old HTTP <c>GetIdentityAsync</c>.</summary>
    public async Task<IdentityInfo?> GetIdentityAsync()
    {
        if (!_crypto.HasKey) return null;
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            var reply = await nest.Account().Get().ConfigureAwait(false);
            return new IdentityInfo(reply.actorId, reply.handle, _nestUrl);
        }
        catch
        {
            return new IdentityInfo(_crypto.ActorIdHex, null, _nestUrl);
        }
    }

    /// <summary><c>fauna.quota.get</c> — tier-aware usage breakdown.</summary>
    public async Task<FfiQuotaGetReply> QuotaGetAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Account().QuotaGet().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> RefreshRegionPlaneAsync(FfiRegionPlane plane, bool onlyIfDue)
    {
        // The plane's relay ask rides the session's connected FfiNestClient — the
        // same one requester every other read uses (libs/fauna-ffi/src/region.rs).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return onlyIfDue
            ? await plane.RefreshIfDue(nest).ConfigureAwait(false)
            : await plane.Refresh(nest).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiFeatureRow>> FeaturesRowsAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Features().Rows().ConfigureAwait(false);
    }

    public async Task<bool> AmIAdminAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Account().AmIAdmin().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiHostAddressOutcome> ReportHostAddressAsync()
    {
        // Thin pass-through to the shared report_host_address FFI fn (the native
        // twin of linux's direct fauna_client_dns::host_address::report_host_address
        // drive). The dial-address classify + never-publish-a-private-address
        // safety all live in the shared fn — no client logic here (priority #2).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.ReportHostAddress(nest).ConfigureAwait(false);
    }

    /// <summary><c>fauna.account.delete</c> — queue account deletion as a
    /// pending action with a cancellation window.</summary>
    public async Task AccountDeleteAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Account().Delete().ConfigureAwait(false);
    }

    /// <summary><c>fauna.profile.handle.change</c> — queue a handle change as a
    /// pending action (delayed + cancellable). Rides the authenticated bearer
    /// connection; the reply (pending-action id / execute-after) is discarded —
    /// the displayed handle updates on the next account refresh once the pending
    /// action executes.</summary>
    public async Task ChangeHandleAsync(string handle)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Account().ChangeHandle(handle).ConfigureAwait(false);
    }

    /// <summary><c>fauna.pending_actions.list</c> — narrowed to still-`pending`
    /// rows client-side (the wire reply carries every status), mirroring every
    /// other app's own filter (`settings.md` § Pending actions).</summary>
    public async Task<IReadOnlyList<FfiPendingActionSummary>> PendingActionsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var rows = await nest.Account().PendingActionsList().ConfigureAwait(false);
        return rows.Where(r => r.status == "pending").ToList();
    }

    /// <summary><c>fauna.pending_actions.cancel</c> — cancel a scheduled
    /// action before it executes.</summary>
    public async Task PendingActionCancelAsync(long id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Account().PendingActionCancel(id).ConfigureAwait(false);
    }

    // ── fauna.bridges.* ─────────────────────────────────────────────────

    /// <summary><c>fauna.bridges.list</c> → <see cref="BridgeInfo"/> rows. The
    /// FFI <c>identity</c> record collapses to its display string (the app model
    /// carries a single label, as the old HTTP shape did).</summary>
    public async Task<IReadOnlyList<BridgeInfo>> BridgesListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var bridges = await nest.Bridges().List().ConfigureAwait(false);
        return bridges
            .Select(b => new BridgeInfo(
                b.id, b.name, b.available, b.linked, b.identity?.display, b.mode,
                (b.linkModes ?? System.Array.Empty<uniffi.fauna_ffi.FfiBridgeLinkMode>())
                    .Select(m => new BridgeLinkMode(
                        m.mode, m.label, m.clientAction, m.platform,
                        m.fields
                            .Select(f => new BridgeLinkField(f.key, f.label, f.fieldType, f.placeholder))
                            .ToList()))
                    .ToList(),
                b.settings
                    .Select(s => new BridgeSetting(
                        s.key, s.label, s.settingType,
                        s.value is FfiCborValue.Bool bo ? bo.@v : (bool?)null,
                        s.value is FfiCborValue.Text tx ? tx.@v : null,
                        s.value is FfiCborValue.Integer iv ? iv.@v : (long?)null))
                    .ToList(),
                b.error))
            .ToList();
    }

    /// <summary><c>fauna.bridges.link</c> — composes <paramref name="fields"/>
    /// into the <c>params</c> CBOR map (text values; typed values are the
    /// deferred link-form reconciliation). The reply is discarded.</summary>
    public async Task BridgesLinkAsync(string bridgeId, string mode, IReadOnlyDictionary<string, string> fields)
    {
        var entries = fields
            .Select(kv => new FfiCborEntry(kv.Key, new FfiCborValue.Text(kv.Value)))
            .ToArray();
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Bridges().Link(bridgeId, mode, new FfiCborValue.Map(entries)).ConfigureAwait(false);
    }

    public async Task BridgesUnlinkAsync(string bridgeId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Bridges().Unlink(bridgeId).ConfigureAwait(false);
    }

    /// <summary><c>fauna.bridges.set_settings</c> — the partial settings map is
    /// composed into the CBOR map the provider's all-optional wire type
    /// deserializes.</summary>
    public async Task BridgesSetSettingsAsync(string bridgeId, IReadOnlyList<BridgeSettingValue> settings)
    {
        var entries = settings
            .Select(s => new FfiCborEntry(
                s.Key,
                s.BoolValue is bool b ? new FfiCborValue.Bool(b)
                    : s.NumberValue is long n ? (FfiCborValue)new FfiCborValue.Integer(n)
                    : new FfiCborValue.Text(s.TextValue ?? string.Empty)))
            .ToArray();
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Bridges().SetSettings(bridgeId, new FfiCborValue.Map(entries)).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<BridgeFollow>> BridgesListFollowsAsync(string bridgeId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var follows = await nest.Bridges().ListFollows(bridgeId).ConfigureAwait(false);
        return follows.Select(f => new BridgeFollow(f.id, f.petname)).ToList();
    }

    public async Task BridgesAddFollowAsync(string bridgeId, string id, string? petname)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Bridges().AddFollow(bridgeId, id, petname, null).ConfigureAwait(false);
    }

    public async Task BridgesRemoveFollowAsync(string bridgeId, string followId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Bridges().RemoveFollow(bridgeId, followId).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<BridgeFeedSubscription>> BridgesFeedsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var subs = await nest.Bridges().FeedsList().ConfigureAwait(false);
        return subs.Select(s => new BridgeFeedSubscription(s.id, s.bridge, s.feedUri, s.name)).ToList();
    }

    public async Task BridgesFeedsCreateAsync(string bridge, string feedUri, string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Bridges().FeedsCreate(bridge, feedUri, name).ConfigureAwait(false);
    }

    public async Task BridgesFeedsDeleteAsync(long id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Bridges().FeedsDelete(id).ConfigureAwait(false);
    }

    // ── fauna.nostr.bunker.* ────────────────────────────────────────────
    //
    // Reached through `FfiNestClient.NostrBunker()` — the same UniFFI façade
    // android (first) and apple (second) consume; linux and tui call the
    // underlying `fauna_client_nostr::NostrBunkerClient` directly, being native
    // Rust. windows is the third UniFFI-consuming client for this plane.
    // Each method is a pure rename into the app model — no interpretation.

    /// <inheritdoc />
    public async Task<BunkerInvite> NostrBunkerCreateInviteAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var reply = await nest.NostrBunker().CreateInvite().ConfigureAwait(false);
        return new BunkerInvite(
            reply.connectionId, reply.connectString, reply.signerPubkey, reply.expiresAt);
    }

    /// <inheritdoc />
    public async Task<bool> NpubConfirmationOwedAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.NpubConfirmationOwed(nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task ConfirmNpubAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.ConfirmNpub(
            nest, _crypto.SecretBytes, DateTimeOffset.UtcNow.ToUnixTimeSeconds()).ConfigureAwait(false);
    }

#if PAYMENTS
    /// <inheritdoc />
    public async Task<IReadOnlyList<ZapSignerEntry>> NostrZapSignersListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var entries = await nest.NostrZapSigners().List().ConfigureAwait(false);
        return entries
            .Select(e => new ZapSignerEntry(e.id, e.signerPubkey, e.label, e.createdAt))
            .ToList();
    }

    /// <inheritdoc />
    public async Task<ZapSignerEntry> NostrZapSignersAddAsync(string signerPubkey, string label)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var e = await nest.NostrZapSigners().Add(signerPubkey, label).ConfigureAwait(false);
        return new ZapSignerEntry(e.id, e.signerPubkey, e.label, e.createdAt);
    }

    /// <inheritdoc />
    public async Task<bool> NostrZapSignersRemoveAsync(string signerPubkey)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.NostrZapSigners().Remove(signerPubkey).ConfigureAwait(false);
    }
#endif

    // ── fauna.email.* ───────────────────────────────────────────────────

    public async Task<IReadOnlyList<FfiEmailFilter>> EmailFiltersListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Email().FiltersList().ConfigureAwait(false);
    }

    public async Task EmailFiltersCreateAsync(
        string name, IReadOnlyList<FfiEmailFilterRule> rules,
        string combination, FfiEmailFilterAction action, int priority)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Email()
            .FiltersCreate(name, rules.ToArray(), combination, action, priority)
            .ConfigureAwait(false);
    }

    public async Task EmailFiltersDeleteAsync(long id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Email().FiltersDelete(id).ConfigureAwait(false);
    }

    /// <summary><c>fauna.email.filters.get</c> — a fresh single-row fetch for the
    /// edit dialog (never the cached list row, which the panel's poll may have
    /// staled).</summary>
    public async Task<FfiEmailFilter> EmailFiltersGetAsync(long id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Email().FiltersGet(id).ConfigureAwait(false);
    }

    /// <summary><c>fauna.email.filters.update</c> — overwrite an existing filter
    /// row in place (the edit dialog's save).</summary>
    public async Task EmailFiltersUpdateAsync(
        long id, string name, IReadOnlyList<FfiEmailFilterRule> rules,
        string combination, FfiEmailFilterAction action, int priority)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Email()
            .FiltersUpdate(id, name, rules.ToArray(), combination, action, priority)
            .ConfigureAwait(false);
    }

    // ── fauna.knocks.* / fauna.contacts.* / fauna.inbox.mode.* ──────────

    /// <summary><c>fauna.knocks.list</c> → <see cref="KnockInfo"/> rows. The FFI
    /// <c>sender</c> hex becomes <see cref="KnockInfo.ActorId"/> (the
    /// accept/block/dismiss key); the roster carries no handle so it stays null.</summary>
    public async Task<IReadOnlyList<KnockInfo>> KnocksListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var knocks = await nest.Contacts().KnocksList().ConfigureAwait(false);
        return knocks
            .Select(k => new KnockInfo(k.sender, k.summary, (ulong)k.createdAt))
            .ToList();
    }

    public async Task KnocksAcceptAsync(string peerId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Contacts().KnocksAccept(peerId).ConfigureAwait(false);
    }

    public async Task KnocksBlockAsync(string peerId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Contacts().KnocksBlock(peerId).ConfigureAwait(false);
    }

    public async Task KnocksUnblockAsync(string peerId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Contacts().KnocksUnblock(peerId).ConfigureAwait(false);
    }

    public async Task KnocksDismissAsync(string peerId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Contacts().KnocksDismiss(peerId).ConfigureAwait(false);
    }

    /// <summary>
    /// Compose the canonical signed <c>(ContactRequest, Post)</c> tuple for an
    /// add-contact knock to <paramref name="recipientActorId"/> (64-hex), via the shared
    /// builder <c>FaunaFfiMethods.BuildKnockPayload</c> (<c>libs/fauna-client-core::email::build_knock_payload</c>
    /// — priority #2/#4: the <c>"Knock"</c>/<c>"Contact request"</c> wire-sentinel is a
    /// protocol value owned by one Rust const, NOT a per-app literal). Static + internal
    /// so the compose step is unit-testable against the real FFI without a live nest (the
    /// transport leg in <see cref="SendKnockAsync"/> needs a connection). Mirrors linux
    /// <c>build_and_send</c>, which consumes the same shared builder.
    /// </summary>
    internal static byte[] BuildKnockPayload(ICryptoService crypto, string recipientActorId, string nodeUrl) =>
        FaunaFfiMethods.BuildKnockPayload(
            crypto.SecretBytes,
            Convert.FromHexString(recipientActorId),
            nodeUrl);

    public async Task SendKnockAsync(string actorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var payload = BuildKnockPayload(_crypto, actorId, _nestUrl);
        // recipient_nest_url null ⇒ same-nest local delivery, faithful to the retired
        // POST /api/v1/inbox/{actor} twin (which only reached same-nest recipients).
        // Cross-nest (Some(peer)) awaits client-side peer discovery (federation.md).
        await nest.Inbox().Send(actorId, null, payload).ConfigureAwait(false);
    }

    /// <summary><c>fauna.contacts.list</c> → <see cref="ContactInfo"/> rows. The
    /// FFI status string maps 1:1 to the app enum:
    /// "pending"→Pending, "accepted"→Accepted, "confirmed"→Confirmed,
    /// "blocked"→Blocked (any other value falls back to Accepted, the safe
    /// good-standing default). <c>confirmed</c> is kept distinct — it was
    /// previously collapsed into Accepted, which dropped the distinct "Confirmed"
    /// status badge (contacts.md § Where logic lives → Status badge text). The
    /// enriched <c>FfiContactItem</c> now carries <c>handle</c>/<c>domain</c>
    /// (contacts.md § State &amp; data shape — <c>ContactSummary</c>), seeded onto the
    /// row so the shared roster filter matches on handle + domain + actor-id.</summary>
    public async Task<IReadOnlyList<ContactInfo>> ContactsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var contacts = await nest.Contacts().ContactsList().ConfigureAwait(false);
        return contacts
            .Select(c => new ContactInfo(
                c.peerId,
                c.handle,
                c.status.ToLowerInvariant() switch
                {
                    "pending" => ContactStatus.Pending,
                    "confirmed" => ContactStatus.Confirmed,
                    "blocked" => ContactStatus.Blocked,
                    _ => ContactStatus.Accepted,
                },
                (ulong?)c.acceptedAt) { Domain = c.domain })
            .ToList();
    }

    public async Task ContactsConfirmAsync(string peerId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Contacts().ContactsConfirm(peerId).ConfigureAwait(false);
    }

    public async Task<string> InboxModeGetAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Contacts().InboxModeGet().ConfigureAwait(false);
    }

    public async Task InboxModeSetAsync(string mode)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Contacts().InboxModeSet(mode).ConfigureAwait(false);
    }

    // ── Post-succession member review ───────────────────────────────────
    // Thin pass-through to the shared FFI free-fns (succession-aftermath.md
    // § Propagation → *Removing a flagged member*). Owner secret from
    // session crypto, never the page — uniform with the destination-CRUD
    // and destination-places methods above.

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiMemberReview>> MemberReviewsListAsync()
    {
        return await FaunaFfiMethods.MemberReviewsList().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> MemberReviewKeepAsync(byte[] person)
    {
        return await FaunaFfiMethods.MemberReviewKeep(person).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<long>> FilterMarksListAsync()
    {
        return await FaunaFfiMethods.FilterMarksList().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> FilterMarkKeepAsync(long filterId)
    {
        return await FaunaFfiMethods.FilterMarkKeep(filterId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> FilterMarkRemovedAsync(long filterId)
    {
        return await FaunaFfiMethods.FilterMarkRemoved(filterId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<CrossGroupEviction> MemberReviewRemoveAsync(ConversationsManager manager, byte[] person)
    {
        return await FaunaFfiMethods.MemberReviewRemove(manager, person).ConfigureAwait(false);
    }

    // ── fauna.notifications.* ───────────────────────────────────────────

    /// <summary><c>fauna.notifications.list</c> — the raw FFI page reply; the
    /// notifications VM maps the items + drives its cursor / has-more off it.</summary>
    public async Task<FfiNotifListReply> NotificationsListAsync(long? cursor, long? limit)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Notifications().List(cursor, limit).ConfigureAwait(false);
    }

    public async Task<long> NotificationsMarkReadAsync(long? upTo)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Notifications().MarkRead(upTo).ConfigureAwait(false);
    }

    public async Task<long> NotificationsCountAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Notifications().Count().ConfigureAwait(false);
    }

    // ── fauna.admin.* ────────────────────────────────────────────────────

    public async Task<FfiAdminStats> AdminStatsAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().Stats().ConfigureAwait(false);
    }

    public async Task<FfiAdminStatus> AdminStatusAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().Status().ConfigureAwait(false);
    }

    public async Task<FfiAdminUsersListReply> AdminUsersListAsync(long? limit, long offset)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().UsersList(limit, offset).ConfigureAwait(false);
    }

    public async Task<FfiAdminUser[]> AdminUsersListAllAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().UsersListAll().ConfigureAwait(false);
    }

    public async Task AdminUsersUpdateAsync(byte[] actorId, string tier, string label)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().UsersUpdate(actorId, tier, label).ConfigureAwait(false);
    }

    public async Task AdminUsersEvictAsync(byte[] actorId, string reason, string category)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().UsersEvict(actorId, reason, category).ConfigureAwait(false);
    }

    public async Task AdminUsersSuspendAsync(byte[] actorId, string reason, string category)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().UsersSuspend(actorId, reason, category).ConfigureAwait(false);
    }

    public async Task AdminUsersCancelEvictionAsync(byte[] actorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().UsersCancelEviction(actorId).ConfigureAwait(false);
    }

    public async Task AdminAdminsAddAsync(byte[] actorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().AdminsAdd(actorId).ConfigureAwait(false);
    }

    public async Task AdminAdminsRemoveAsync(byte[] actorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().AdminsRemove(actorId).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiAdminUser>> AdminEvictionsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().EvictionsList().ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiAdminTier>> AdminTiersListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().TiersList().ConfigureAwait(false);
    }

    public async Task AdminTiersUpdateAsync(
        string name, long maxInboxBytes, long maxStorageBytes, long maxDevices, long maxBlobSize, long maxFeeds)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin()
            .TiersUpdate(name, maxInboxBytes, maxStorageBytes, maxDevices, maxBlobSize, maxFeeds)
            .ConfigureAwait(false);
    }

    public async Task AdminSetRegistrationModeAsync(FfiRegistrationMode mode, ulong? maxFreeUsers)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().SetRegistrationMode(mode, maxFreeUsers).ConfigureAwait(false);
    }

    public async Task AdminSetAgeVerificationRequiredAsync(bool required)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().SetAgeVerificationRequired(required).ConfigureAwait(false);
    }

    public async Task AdminUsersCreateAsync(byte[] actorId, string tier, string? handle)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().UsersCreate(actorId, tier, handle).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiAdminMembershipTier>> AdminMembershipTiersListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().MembershipTiersList().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task AdminMembershipTiersSetAsync(string tierName, string adminTier, string lapseTier)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().MembershipTiersSet(tierName, adminTier, lapseTier).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task AdminMembershipTiersClearAsync(string tierName)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().MembershipTiersClear(tierName).ConfigureAwait(false);
    }

    public async Task<FfiSetupStatus> SetupStatusAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.SetupStatus().ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiAdminInviteCode>> AdminInviteCodesListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().InviteCodesList().ConfigureAwait(false);
    }

    public async Task<string> AdminInviteCodesCreateAsync(string code, string tier, long uses, byte[]? guardianActorId = null, string? ageBand = null)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().InviteCodesCreate(code, tier, uses, guardianActorId, ageBand).ConfigureAwait(false);
    }

    public async Task AdminInviteCodesDeleteAsync(string code)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().InviteCodesDelete(code).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiAdminInviteRequest>> AdminInviteRequestsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().InviteRequestsList().ConfigureAwait(false);
    }

    public async Task<FfiAdminInviteRequestApproveReply> AdminInviteRequestsApproveAsync(long id, string? tier, string? label, byte[]? guardianActorId = null, string? ageBand = null)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().InviteRequestsApprove(id, tier, label, guardianActorId, ageBand).ConfigureAwait(false);
    }

    public async Task AdminInviteRequestsDenyAsync(long id, string? reason)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().InviteRequestsDeny(id, reason).ConfigureAwait(false);
    }

    public async Task<FfiAdminServiceFlags> AdminServicesListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().ServicesList().ConfigureAwait(false);
    }

    public async Task AdminServicesUpdateAsync(string name, bool enabled)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().ServicesUpdate(name, enabled).ConfigureAwait(false);
    }

    /// <summary><c>fauna.admin.set_serving_port</c> — the Admin-class node-policy
    /// write, via the shared <c>FfiAdminClient.SetServingPort</c> (one shared
    /// <c>AdminClient::set_serving_port</c> exposed at each boundary, not a
    /// per-app raw call). The read-back rides <c>fauna.setup.status</c>.</summary>
    public async Task SetServingPortAsync(ushort port)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().SetServingPort(port).ConfigureAwait(false);
    }

    /// <summary><c>fauna.admin.request_host_restart</c> — the admin "restart now"
    /// flag write, via the shared <c>FfiAdminClient.RequestHostRestart</c> (one
    /// shared <c>AdminClient::request_host_restart</c> exposed at each boundary, not
    /// a per-app raw call). A <c>no_host</c> rejection propagates to the VM.</summary>
    public async Task RequestHostRestartAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().RequestHostRestart().ConfigureAwait(false);
    }

    /// <summary><c>fauna.admin.region.get</c> via the shared
    /// <c>fauna_client_admin::admin_region_view</c> fold — a free FFI function
    /// (not <c>nest.Admin()</c>), the native twin of apple's
    /// <c>adminRegionStatus()</c> wrapper.</summary>
    public async Task<FfiAdminRegionView> AdminRegionStatusAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.AdminRegionStatus(nest).ConfigureAwait(false);
    }

    /// <summary><c>fauna.admin.region.set</c> — declare (<paramref name="region"/>
    /// non-null) or withdraw (null), via the same free FFI function
    /// <see cref="AdminRegionStatusAsync"/> pairs with.</summary>
    public async Task SetRegionAsync(string? region)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.AdminSetRegion(nest, region).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<LogEntry>> AdminLogsAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().Logs().ConfigureAwait(false);
    }

    public async Task<FfiAdminHostingRow[]> AdminCustodyHostingListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().CustodyHostingList().ConfigureAwait(false);
    }

    public async Task<FfiAdminHostingRemoveReply> AdminCustodyHostingRemoveAsync(string hostActorId, byte[] grantId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().CustodyHostingRemove(hostActorId, grantId).ConfigureAwait(false);
    }

    /// <summary>The shared <c>seed_rotation_confirm_view</c> fold — a free FFI
    /// function over the CONNECTED client (not a throwaway
    /// <c>FfiNestClient</c>: this account's own session already holds a live
    /// connection, and the fold needs no local creds beyond it).</summary>
    public async Task<FfiSeedRotationConfirmView> SeedRotateRosterAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.SeedRotateRoster(nest).ConfigureAwait(false);
    }

    /// <summary>The rotation ceremony itself, over the connected client + this
    /// account's own secret bytes (the same pairing
    /// <see cref="RunSuccessionAftermathAsync"/> / <see cref="CustodyFacetLoadAsync"/>
    /// already use).</summary>
    public async Task<FfiSeedRotationResult> RotateDeploymentSeedAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.RotateDeploymentSeed(nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiDeploymentSeedSelfHeal> SelfHealDeploymentSeedCustodyAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.SelfHealDeploymentSeedCustody(nest, _crypto.SecretBytes, BackupPaths.DataDir).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task RunCriticalAlertSweepAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.RunCriticalAlertSweep(nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task RunCriticalAlertSweepLoopAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.RunCriticalAlertSweepLoop(nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task ApplyPostClaimServingEnablementAsync(
        string nodeUrl, bool email, bool caldav, bool carddav, bool webdav)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.ApplyPostClaimServingEnablement(
            nest, _crypto.SecretBytes, nodeUrl, email, caldav, carddav, webdav).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task RecoveryRegisterDeferredKitAsync(string kitHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.RecoveryRegisterDeferredKit(nest, _crypto.SecretBytes, kitHex).ConfigureAwait(false);
    }

    /// <summary>The outside-app sign-in key set (<c>admin-nest-oauth-*</c>,
    /// authorization-server.md § The issuer → Two rotation arms). A free FFI
    /// function over the connected client, same shape as
    /// <see cref="AdminRegionStatusAsync"/>.</summary>
    public async Task<FfiIssuerKeyView> AdminIssuerKeyStatusAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.AdminIssuerKeyStatus(nest).ConfigureAwait(false);
    }

    /// <summary>The ordinary sign-in-key rotation.</summary>
    public async Task<uniffi.fauna_core.LocalizedText> AdminRotateIssuerKeyAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.AdminRotateIssuerKey(nest).ConfigureAwait(false);
    }

    /// <summary>The armed forced arm (issuer key or session secret).</summary>
    public async Task<uniffi.fauna_core.LocalizedText> AdminForceRotateIssuerAsync(FfiIssuerForcedArm arm)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.AdminForceRotateIssuer(nest, arm).ConfigureAwait(false);
    }

    // ── fauna.spam.* (Privacy + Moderation pages) ───────────────────────

    public async Task<FfiSpamPreferences> SpamGetPreferencesAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Spam().GetPreferences().ConfigureAwait(false);
    }

    public async Task<FfiSpamPreferences> SpamSetPreferencesAsync(
        ushort? spamThreshold, ushort? phishingThreshold)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Spam()
            .SetPreferences(spamThreshold, phishingThreshold)
            .ConfigureAwait(false);
    }

    // ── fauna.moderation.* (Moderation page queue + train) ──────────────

    public async Task<IReadOnlyList<FfiObligationAction>> ModerationActionsAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Moderation().Actions().ConfigureAwait(false);
    }

    public async Task ModerationTrainAsync(string contentId, string verdict)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Moderation().Train(contentId, verdict).ConfigureAwait(false);
    }

    public async Task<bool> ModerationReportShareSetAsync(bool share)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Moderation().ReportShareSet(share).ConfigureAwait(false);
    }

    public async Task<FfiReportShareStatus> ModerationReportShareStatusAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Moderation().ReportShareStatus().ConfigureAwait(false);
    }

    public async Task<string> ModerationLegalTakedownAsync(
        string contentId, bool conversation, string legalReference, bool restore)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Moderation()
            .LegalTakedown(contentId, conversation, legalReference, restore)
            .ConfigureAwait(false);
    }

    // ── fauna.moderation.abuse_report.* (user-initiated reporting) ──────

    public async Task<FfiReportSent> AbuseReportSubmitAsync(FfiReportTarget target, FfiReportForm form)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Moderation().AbuseReportSubmit(target, form).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiReportLedgerRow>> AbuseReportMineAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Moderation().AbuseReportMine().ConfigureAwait(false);
    }

    public async Task AbuseReportWithdrawAsync(string reportId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Moderation().AbuseReportWithdraw(reportId).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiReportQueueRow>> AbuseReportQueueAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().AbuseReportQueue().ConfigureAwait(false);
    }

    public async Task AbuseReportResolveAsync(string reportId, bool acted)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Admin().AbuseReportResolve(reportId, acted).ConfigureAwait(false);
    }

    public Task<string[]> HideReportedAsync(string id) => FaunaFfiMethods.HideReported(id);

    public Task<string[]> LoadHiddenContentAsync() => FaunaFfiMethods.LoadHiddenContent();

    public async Task<uint?> SpamThresholdOverrideGetAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.SpamThresholdOverrideGet(nest).ConfigureAwait(false);
    }

    public async Task<uint?> SpamThresholdOverrideSetAsync(uint? value)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.SpamThresholdOverrideSet(nest, value).ConfigureAwait(false);
    }

    // ── fauna.family.* (Family page) ────────────────────────────────────

    public async Task<FfiFamilyStatus> FamilyStatusAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var status = await nest.Family().Status().ConfigureAwait(false);
        // Persist the last-known supervision snapshot on every successful read
        // (family-safety.md § Content policy, clause 2) — keyed on the SESSION's
        // own actor, never the registry's active pointer (a mid-switch transient
        // can move it first), mirroring android's ApiClient.familyStatus. Best-
        // effort: no account registry wired (unit tests, some minimal e2e paths)
        // degrades to a silent no-op rather than failing the status read itself.
        _accountRegistry?.Invoke().PersistSupervisionSnapshot(_crypto.ActorIdHex, status);
        return status;
    }

    /// <inheritdoc />
    public FfiSupervisionSnapshot? SupervisionSnapshot(string actorId) =>
        _accountRegistry?.Invoke().SupervisionSnapshot(actorId);

    public async Task FamilyPolicyUpdateAsync(byte[] supervisedActorId, FfiReachPolicy policy)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().PolicyUpdate(supervisedActorId, policy).ConfigureAwait(false);
    }

    public async Task FamilyNotifyReportAsync(FfiFamilyContentNotice[] entries, int utcOffsetMinutes)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().NotifyReport(entries, utcOffsetMinutes).ConfigureAwait(false);
    }

    public async Task<FfiFamilyUsageReport> FamilyUsageReportAsync(uint minutes, int utcOffsetMinutes)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Family().UsageReport(minutes, utcOffsetMinutes).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiFamilyApprovalEntry>> FamilyApprovalsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Family().ApprovalsList().ConfigureAwait(false);
    }

    public async Task FamilyApprovalsDecideAsync(byte[] supervisedActorId, string kind, byte[] peerActorId, byte[] messageId, string bridgeId, string operation, string target, string peerAddress, bool approve)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().ApprovalsDecide(supervisedActorId, kind, peerActorId, messageId, bridgeId, operation, target, peerAddress, approve).ConfigureAwait(false);
    }

    public async Task FamilyContactAddAsync(byte[] supervisedActorId, byte[] peerActorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().ContactAdd(supervisedActorId, peerActorId).ConfigureAwait(false);
    }

    public async Task FamilyGraduateAsync(byte[] supervisedActorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().Graduate(supervisedActorId).ConfigureAwait(false);
    }

    public async Task FamilyTransferAsync(byte[] supervisedActorId, byte[] newGuardianActorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().Transfer(supervisedActorId, newGuardianActorId).ConfigureAwait(false);
    }

    public async Task FamilyTransferAcceptAsync(byte[] supervisedActorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().TransferAccept(supervisedActorId).ConfigureAwait(false);
    }

    public async Task FamilyTransferDeclineAsync(byte[] supervisedActorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().TransferDecline(supervisedActorId).ConfigureAwait(false);
    }

    public async Task FamilyTransferCancelAsync(byte[] supervisedActorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().TransferCancel(supervisedActorId).ConfigureAwait(false);
    }

    public async Task FamilyDeviceMarkAsync(byte[] supervisedActorId, string deviceId, bool marked)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Family().DeviceMark(supervisedActorId, deviceId, marked).ConfigureAwait(false);
    }

    // ── fauna.folders.* (Media page) ──────────────────────────────────

    public async Task<IReadOnlyList<FfiFolder>> FoldersListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Folders().List().ConfigureAwait(false);
    }

    public async Task<FfiFolder> FoldersCreateAsync(string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        // A create carries no selective-sync include/exclude paths (they seal
        // under the row id the create mints; the Devices page's wizard sets
        // them by a later update).
        return await nest.Folders()
            .Create(name, null)
            .ConfigureAwait(false);
    }

    public async Task<FfiSealBackfillSweepReport> RunSealBackfillSweepAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Folders().RunSealBackfillSweep().ConfigureAwait(false);
    }

    // ── Cross-user sharing (owner side) — folders.md § Sharing ────────

    public async Task<IReadOnlyList<FfiFolderActorMember>> FoldersMembersListActorsAsync(string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Folders().MembersListActors(name).ConfigureAwait(false);
    }

    public async Task FoldersSetMemberAccessAsync(string name, string actorIdHex, string access, long? byteCap)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Folders().MembersSetAccess(name, actorIdHex, access, byteCap).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiFolderDevice>> FoldersDevicesAsync(string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Folders().Devices(name).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiFolderMember>> FoldersPlaceRowsAsync(
        string name, IReadOnlyList<DeviceSummary> devices)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Folders().PlaceRows(name, devices.ToArray()).ConfigureAwait(false);
    }

    /// <param name="access">The invited member's grant (multi-writer Phase 1):
    /// <c>"writer"</c> for a read-write share, <c>null</c>/<c>"reader"</c> for the
    /// read-only default. Defaulted to <c>null</c> here so windows keeps its current
    /// read-only sharing behavior unchanged — the writer-grant UI belongs to the
    /// shared-folders track, not to this call-site update.</param>
    public async Task<FfiShareOutcome> FoldersShareAsync(ConversationsSession session, string name, byte[] memberId, string? memberNestUrl, string? access = null)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        // access: defaults to null = the read-only reader default (pre-Phase-1 semantics),
        // which is exactly what windows did before `folders_share` grew the parameter.
        // The share-time role select + the writer grant are the multi-writer Phase 1
        // follow-on, owed by every app alike (folders.md § Sharing) — apple passes
        // nil here for the same reason. Threaded as a parameter rather than hardcoded so
        // that follow-on only has to add the UI, not re-open this seam.
        return await FaunaFfiMethods.FoldersShare(nest, session, _crypto.SecretBytes, name, memberId, memberNestUrl, access).ConfigureAwait(false);
    }

    public async Task<FfiRemoveOutcome> FoldersRemoveMemberAsync(ConversationsSession session, string name, byte[] channelId, byte[] memberId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.FoldersRemoveMember(nest, session, _crypto.SecretBytes, name, channelId, memberId).ConfigureAwait(false);
    }

    public async Task<uint> FoldersServeSetAsync(ConversationsSession session, string name, string? mlsGroupIdHex, bool enable)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.FoldersServeSet(nest, session, _crypto.SecretBytes, name, mlsGroupIdHex, enable).ConfigureAwait(false);
    }

    public async Task<bool> FoldersCanServeWebdavAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.FoldersCanServeWebdav(nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    public async Task FoldersPaywallSetAsync(ConversationsSession session, string name, string? mlsGroupIdHex, string tier)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.FoldersPaywallSet(nest, session, _crypto.SecretBytes, name, tier, mlsGroupIdHex).ConfigureAwait(false);
    }

    // ── Cross-user sharing (recipient side) — folders.md § Sharing ────

    public async Task FoldersLeaveAsync(ConversationsSession session, string groupIdHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.FoldersLeave(nest, session, groupIdHex).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<FfiPendingShare>> FoldersPendingSharesAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.FoldersPendingShares(nest).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiFollowedFolder>> FoldersFollowPublicAsync(
        string owner, string folderName)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods
            .FoldersFollowPublic(nest, _crypto.SecretBytes, owner, folderName)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiFollowedFolder>> FoldersUnfollowPublicAsync(
        string homeNestUrl, long folderId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods
            .FoldersUnfollowPublic(nest, _crypto.SecretBytes, homeNestUrl, folderId)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task WireDevicesFollowedFoldersAsync(DevicesMachine devices)
    {
        // Best-effort, the exact shape of WireDevicesForeignSetsAsync above:
        // an unwired source means "no follows to show", never a page error.
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            FaunaFfiMethods.WireDevicesFollowedFolders(devices, nest, _crypto.SecretBytes);
        }
        catch (FfiException)
        {
        }
    }

    public async Task FoldersAcceptShareAsync(ConversationsSession session, long inboxId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.FoldersAcceptShare(nest, session, inboxId).ConfigureAwait(false);
    }

    public async Task FoldersDeclineShareAsync(long inboxId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.FoldersDeclineShare(nest, inboxId).ConfigureAwait(false);
    }

#if P2P_SHARE
    // The ceremony doors — see INestRpcClient's P2P_SHARE region.
    public async Task<FfiCeremonySeat> BindOfflineShareSeatAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.OfflineShareBindSeat(nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    public Task<CeremonyStatus> OfflineShareInitiateAsync(FfiCeremonySeat seat, string peerCodeInput) =>
        RetryWhileAccountRuntimeNotReady(async () =>
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            return await FaunaFfiMethods.OfflineShareInitiate(nest, seat, _crypto.SecretBytes, peerCodeInput).ConfigureAwait(false);
        });

    public Task<CeremonyStatus> OfflineShareConsentAsync(FfiCeremonySeat seat, byte[] scopeId) =>
        RetryWhileAccountRuntimeNotReady(async () =>
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            return await FaunaFfiMethods.OfflineShareConsent(nest, seat, _crypto.SecretBytes, scopeId).ConfigureAwait(false);
        });

    /// <summary>Retry <paramref name="call"/> while it fails with the specific
    /// "account runtime not ready yet" refusal
    /// (<c>fauna_sync_engine::offline_share::{initiate,consent}</c>'s
    /// <c>account.ok_or_else(|| "this device has no account runtime yet")</c>
    /// guard — the only two ceremony acts that touch the W3 account store).
    /// <c>StartAccountRuntimeAsync</c> is fire-and-forget at login by design
    /// (<c>libs/fauna-ffi/src/account_runtime.rs</c>'s own doc: "Returns as
    /// soon as the spawn lands... every I/O-bound step is inside phase 2" — a
    /// credential-slot read, a store open, an IPC round trip to the co-located
    /// agent), so a ceremony act reached quickly after sign-in — the offline-
    /// share panel's whole point is to be usable within seconds of opening —
    /// can race that assembly. Any OTHER failure (a real dial refusal, a
    /// malformed code, an uninvited-initiator refusal, …) propagates
    /// immediately, unretried: this is not a general retry wrapper, and
    /// widening the match would hide a real ceremony failure behind a
    /// pointless wait.</summary>
    private static async Task<T> RetryWhileAccountRuntimeNotReady<T>(Func<Task<T>> call)
    {
        var deadline = DateTime.UtcNow.AddSeconds(15);
        while (true)
        {
            try { return await call().ConfigureAwait(false); }
            catch (Exception ex) when (
                DateTime.UtcNow < deadline
                && (ex.Message.Contains("no account runtime yet") || ex.Message.Contains("not enrolled yet")))
            {
                await Task.Delay(250).ConfigureAwait(false);
            }
        }
    }

    public async Task OfflineShareDeclineAsync(FfiCeremonySeat seat, byte[] scopeId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.OfflineShareDecline(nest, seat, _crypto.SecretBytes, scopeId).ConfigureAwait(false);
    }

    public async Task<FfiGroupShareViews> OfflineShareLoadGroupSharesAsync(FfiCeremonySeat? seat = null)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.OfflineShareLoadGroupShares(nest, _crypto.SecretBytes, seat).ConfigureAwait(false);
    }
#endif

    // ── fauna.search.query (Search page) ────────────────────────────────

    public async Task<IReadOnlyList<FfiSearchResult>> SearchQueryAsync(
        string query, string? contentType, long? limit, long? offset)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        // before/after date cursors are unused by the Search page (no date filter UI).
        return await nest.Search()
            .Query(query, contentType, null, null, limit, offset)
            .ConfigureAwait(false);
    }

    // ── fauna.filesync.snapshot.* / fauna.sync.backup_status (Backups page) ──

    public async Task<IReadOnlyList<FolderStatus>> BackupStatusAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var entries = await nest.Sync().BackupStatus().ConfigureAwait(false);
        return entries
            .Select(e => new FolderStatus(e.name, e.lastChangeAt is { } t ? (ulong)t : (ulong?)null))
            .ToList();
    }

    // ── fauna.sync.conflicts.* (Sync Conflicts page) ────────────────────

    public async Task<IReadOnlyList<ConflictInfo>> ConflictsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var conflicts = await nest.Sync().ConflictsList().ConfigureAwait(false);
        // The page renders path / type / details / created-at; folder, device
        // id, and candidate versions are dropped (mark-only resolve needs none).
        return conflicts
            .Select(c => new ConflictInfo(c.id, c.path, c.conflictType, c.details, (ulong)c.createdAt))
            .ToList();
    }

    public async Task<IReadOnlyList<SnapshotInfo>> SnapshotListAsync(string folder)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        // folder mode (message_kind null); 0 = server default page size.
        var rows = await nest.Snapshots().SnapshotList(null, folder, 0).ConfigureAwait(false);
        // Summary rows carry no tags / device id (those live on the detail).
        return rows
            .Select(r => new SnapshotInfo(
                (ulong)r.id, (ulong)r.fileCount, (ulong)r.totalBytes, (ulong)r.createdAt,
                new List<string>(), null))
            .ToList();
    }

    public async Task<SnapshotDetailInfo> SnapshotGetAsync(ulong id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var r = await nest.Snapshots().SnapshotGet((long)id).ConfigureAwait(false);
        var files = r.files
            .Select(f => new SnapshotFileInfo(f.path, (ulong)f.sizeBytes, f.fileType))
            .ToList();
        return new SnapshotDetailInfo(
            (ulong)r.id, r.folder, (ulong)r.fileCount, (ulong)r.totalBytes, (ulong)r.createdAt,
            r.tags.ToList(), HexOrNull(r.deviceId), files);
    }

    public async Task<SnapshotInfo> SnapshotCreateFolderAsync(string folder, IReadOnlyList<string> tags)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var r = await nest.Snapshots().SnapshotCreateFolder(folder, tags.ToArray()).ConfigureAwait(false);
        return new SnapshotInfo(
            (ulong)r.id, (ulong)r.fileCount, (ulong)r.totalBytes, (ulong)r.createdAt,
            r.tags.ToList(), HexOrNull(r.deviceId));
    }

    public async Task SnapshotDeleteAsync(ulong id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Snapshots().SnapshotDelete((long)id).ConfigureAwait(false);
    }

    public async Task SnapshotPruneAsync(string folder, uint? keepLast, uint? keepDaily, uint? keepWeekly, uint? keepMonthly)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        // keepYearly (added to the shared FfiSnapshotsClient.SnapshotPrune retention
        // model) defaults off here — windows keeps its existing daily/weekly/monthly
        // policy and does not yet expose a yearly tier (the per-app retention-UI
        // lift incl. yearly is a separate backups-area track). Passing null is
        // behavior-preserving vs the pre-yearly model.
        await nest.Snapshots()
            .SnapshotPrune(folder, false, keepLast, keepDaily, keepWeekly, keepMonthly,
                keepYearly: null)
            .ConfigureAwait(false);
    }

    // ── Restore surface ─────────────────────────────────────────────────
    // Thin pass-through to the shared FfiSnapshotsClient restore methods; the
    // VM maps the raw FFI replies to its display rows (backups.md § Where logic
    // lives — render rules are client glue).

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiRestoreHistoryRow>> SnapshotListRestoreHistoryAsync(uint limit = 0)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Snapshots().SnapshotListRestoreHistory(limit).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiRestoreDivergenceRow>> SnapshotListRestoreDivergenceAsync(long snapshotId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Snapshots().SnapshotListRestoreDivergence(snapshotId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiSnapshotRestoreReply> SnapshotRestoreMessageKindAsync(long snapshotId, string confirmId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Snapshots().SnapshotRestoreMessageKind(snapshotId, confirmId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task SnapshotDeleteImmediateAsync(long snapshotId, string confirmId, string acknowledge)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await nest.Snapshots()
            .SnapshotDeleteImmediate(snapshotId, confirmId, acknowledge)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiSnapshotSummary>> MessageKindSnapshotListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        // message-kind mode: folder null, message_kind null (all kinds), 0 = default page.
        return await nest.Snapshots().SnapshotList(null, null, 0).ConfigureAwait(false);
    }

    // ── Backup destinations (management) ────────────────────────────────
    // Thin pass-through to the shared FFI free-fns (backups.md § Where logic
    // lives). The owner secret comes from the session crypto, never the page;
    // each fn loads → resolves (add/edit) → mutates → the plane write and
    // returns the freshly-persisted destination list.

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.BackupDestinationsList(nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationAddAsync(string url, string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.BackupDestinationAdd(nest, _crypto.SecretBytes, url, name).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationEditAsync(string id, string url, string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.BackupDestinationEdit(nest, _crypto.SecretBytes, id, url, name).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationRemoveAsync(string id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.BackupDestinationRemove(nest, _crypto.SecretBytes, id).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationKeepAsync(string id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.BackupDestinationKeep(nest, _crypto.SecretBytes, id).ConfigureAwait(false);
    }

    // ── Folder destination places (backup-destinations.md § Ordinary-folder
    // coverage) ──────────────────────────────────────────────────────────
    // Thin pass-through to the shared FFI free-fns, uniform with the
    // destination CRUD above: owner secret from session crypto, never the page.

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationsListAsync(long folderId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.FolderDestinationsList(nest, _crypto.SecretBytes, folderId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationAttachAsync(long folderId, string destinationId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.FolderDestinationAttach(nest, _crypto.SecretBytes, folderId, destinationId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiFolderDestinationPlace>> FolderDestinationDetachAsync(long folderId, string destinationId, string folderSet)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.FolderDestinationDetach(nest, _crypto.SecretBytes, folderId, destinationId, folderSet).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiBackupDestinationView>> BackupDestinationEnrollCustodianAsync(
        string custodianDeviceId, string name, ulong? capacityCapBytes)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.BackupDestinationEnrollCustodian(
            nest, _crypto.SecretBytes, custodianDeviceId, name, capacityCapBytes).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiBackupDestinationStatus>> BackupDestinationStatusAsync()
    {
        // Secret is supplied here (session crypto), never by the page — uniform with the
        // four CRUD methods above. The nest derives the owner from the authenticated
        // connection, so the read needs nothing else.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.BackupDestinationStatus(
            nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiDestinationAuditRow>> BackupAuditRunPassAsync(
        string statePath, string? syncStateDir)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.BackupAuditRunPass(
            nest, _crypto.SecretBytes, statePath, syncStateDir).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public bool BackupAuditObserve(string statePath, long lastActivityMs)
        => FaunaFfiMethods.BackupAuditObserve(statePath, lastActivityMs);

    /// <inheritdoc />
    public async Task<byte[]> DownloadSnapshotFileBytesAsync(string deviceId, ulong snapshotId, string path)
    {
        // The shared client-side walk, server-side: the file's manifest_hash is
        // resolved inside the FFI and never crosses to C#. Secret from session
        // crypto, deviceId hex → bytes — uniform with BackupDestinationStatusAsync.
        // The windows snapshot surface is ulong throughout; the walk's id is i64
        // (as on the full-restore leg), so the narrowing lives here at the FFI
        // adapter rather than leaking a long into the page/VM.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.DownloadSnapshotFileBytes(
            nest, _crypto.SecretBytes, Convert.FromHexString(deviceId), (long)snapshotId, path)
            .ConfigureAwait(false);
    }

    // ── Muted keywords (moderation.md § Muted keywords) ────────────────────
    // Thin pass-through to the shared FFI free-fns — input-free: the account
    // runtime holds the owner secret and the nest plane, so neither is passed.

    /// <inheritdoc />
    public async Task<MutedWordsSnapshot> MutedKeywordsListAsync() =>
        await FaunaFfiMethods.LoadMutedWords().ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<MutedWordsSnapshot> MutedKeywordsSetAsync(IReadOnlyList<uniffi.fauna_core.MutedKeyword> keywords) =>
        await FaunaFfiMethods.SaveMutedWords(keywords.ToArray()).ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<MutedWordsSnapshot> MutedKeywordsAddAsync(string word) =>
        await FaunaFfiMethods.AddMutedWord(word).ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<MutedWordsSnapshot> MutedKeywordsRemoveAsync(string word) =>
        await FaunaFfiMethods.RemoveMutedWord(word).ConfigureAwait(false);

    // ── Trained topics (topic-factors.md § Authoring surface & picker) ──
    // Thin pass-through to the shared FFI free-fns. The nest client still goes
    // in (the model plane is a nest call); the owner secret does not.

    /// <inheritdoc />
    public async Task<FfiTrainedTopicRow[]> TrainedTopicsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.ListTrainedTopics(nest).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiTrainedTopicRow[]> TrainedTopicsCreateAsync(string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.CreateTrainedTopic(nest, name).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiTrainedTopicRow[]> TrainedTopicsRenameAsync(byte[] id, string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.RenameTrainedTopic(nest, id, name).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiTrainedTopicRow[]> TrainedTopicsDeleteAsync(byte[] id)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.DeleteTrainedTopic(nest, id).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiTrainedTopicRow[]> TrainedTopicsSetLearnFromEngagementAsync(byte[] id, bool on)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.SetTrainedTopicEngagement(nest, id, on).ConfigureAwait(false);
    }

    // ── Publishing a trained factor as a List (topic-factors.md § Publishing
    // a trained factor; frame D8) ───────────────────────────────────────────

    /// <inheritdoc />
    public async Task<ScoredExemplar[]> ScoreCorpusForFactorAsync(string factor) =>
        await SharedFeedManager().ScoreCorpusForFactor(factor).ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<FfiPublishedList> TrainedTopicPublishListAsync(byte[] factorId, string name, IReadOnlyList<FfiPublishEntry> entries)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.TrainedTopicPublishList(nest, _crypto.SecretBytes, factorId, name, entries.ToArray()).ConfigureAwait(false);
    }

    // ── Publishing a trained factor as a Model (topic-factors.md § Publishing
    // a trained factor, v2) ─────────────────────────────────────────────────

    /// <inheritdoc />
    public async Task<TrainedModelReview> ScrubCorpusForFactorAsync(string factor) =>
        await SharedFeedManager().ScrubCorpusForFactor(factor).ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<FfiPublishedModel> TrainedTopicPublishModelAsync(byte[] factorId, string name, uint moreDocs, uint lessDocs, IReadOnlyList<FfiPublishNgram> ngrams)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.TrainedTopicPublishModel(nest, _crypto.SecretBytes, factorId, name, moreDocs, lessDocs, ngrams.ToArray()).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<string?> DefaultConflictPolicyGetAsync() =>
        await FaunaFfiMethods.LoadSyncPrefs().ConfigureAwait(false);

    /// <inheritdoc />
    public async Task<string?> DefaultConflictPolicySetAsync(string? policy) =>
        await FaunaFfiMethods.SaveSyncPrefs(policy).ConfigureAwait(false);

    // ── Subscriptions — profile Tiers-tab SELF author management ─────────────
    // Thin glue over the landed libs/fauna-ffi subscriptions surface (priority #2,
    // no client-side crypto): the three mint-bearing actions go through the author
    // orchestration free-fns (owner_secret from session crypto, uniform with the
    // backup-destination CRUD above), the rest are thin FfiSubscriptionsClient
    // pass-throughs. monetization.md § Pillar 1; apple landed the exposure.

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiTierItem>> SubscriptionTiersListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().TiersList().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> SubscriptionTierCreateAsync(
        string name, uint rank, string? description, string? priceHint, string? paymentUrl, bool autoApprove,
        ulong? askingPriceSats)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.SubscriptionsCreateTier(
            nest, _crypto.SecretBytes, name, rank, description, priceHint, paymentUrl, autoApprove,
            askingPriceSats)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> SubscriptionTierUpdateAsync(
        string name, uint? rank, string? description, string? priceHint, string? paymentUrl, bool? autoApprove,
        ulong? askingPriceSats)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions()
            .TiersUpdate(name, rank, description, priceHint, paymentUrl, autoApprove, askingPriceSats)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> SubscriptionTierDeleteAsync(string name)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().TiersDelete(name).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiPendingRequest>> SubscriptionRequestsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().RequestsList().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiApproveReply> SubscriptionRequestApproveAsync(FfiPendingRequest request)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.SubscriptionsApproveSubscriber(
            nest, _crypto.SecretBytes, request).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> SubscriptionRequestRejectAsync(long requestId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().RequestsReject(requestId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiSubscriberEntry>> SubscriptionSubscribersListAsync(string tierName)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().SubscribersList(tierName).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task SubscriptionSubscriberRemoveAsync(string tierName, byte[] subscriberId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        await FaunaFfiMethods.SubscriptionsRemoveSubscriber(
            nest, _crypto.SecretBytes, tierName, subscriberId).ConfigureAwait(false);
    }

    // ── Subscriptions — author-side reconciliation ────────────────────────
    // The encrypted-mode auto-approve loop (monetization.md § The unifying model
    // grant path 2 + § Pillar 1 → "Where the logic lives"): a thin pass-through to
    // the ONE shared tick, driven by the pump below. The resume/drain pair this
    // used to expose separately is deliberately gone — see INestRpcClient.

    /// <inheritdoc />
    public async Task<FfiReconcilePass> SubscriptionsReconcileOnceAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.SubscriptionsReconcileOnce(
            nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    private Task? _authorPump;
    private readonly CancellationTokenSource _authorPumpCts = new();

    /// <summary>
    /// Start the author-side subscriptions reconciliation pump (idempotent): the
    /// shared tick — resume crash-staged removals, then drain queued
    /// auto-approvals — on connect and on every poll backstop, the loop that
    /// makes an encrypted-mode follow auto-grant with no manual approve
    /// (<see cref="SubscriptionsAuthorPump"/>). Fire-and-forget, instance
    /// lifetime; canceled by <see cref="DisposeAsync"/> (a poll loop has no
    /// FFI watch stream to end it, unlike the pumps above). Call from both
    /// login seams (StartMainAppAsync and the TestAgent set_state login — the
    /// e2e path never reaches StartMainAppAsync). Headless: no UI thread, no
    /// bound state.
    /// </summary>
    public void StartSubscriptionsAuthorPump()
    {
        if (_authorPump is not null) return;
        _authorPump = new SubscriptionsAuthorPump(this).RunAsync(_authorPumpCts.Token);
    }

    // ── Subscriptions — OTHER-profile subscriber browse ──────────────────
    // Thin FfiSubscriptionsClient pass-throughs (subscriber side, no client-side
    // crypto). monetization.md § Pillar 1 surface 2; linux apps/fauna-linux/src/views/profile/offers.rs.

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiTierItem>> SubscriptionOffersListAsync(byte[] authorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().OffersList(authorId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiSubscriptionStatus> SubscriptionStatusGetAsync(byte[] authorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().StatusGet(authorId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiSubscribeReply> SubscriptionSubscribeAsync(byte[] authorId, string tier)
    {
        // Publish the caller's identity-seed ML-KEM ek (surface B, S4b) unconditionally
        // (no capability token), so the author can wrap hybrid KeyBlobs to this
        // subscriber. Twin of the FFI free fn linux
        // calls (offers.rs::subscribe_to / mod.rs::follow).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await uniffi.fauna_ffi.FaunaFfiMethods
            .SubscriptionsSubscribePublishingEk(nest, _crypto.SecretBytes, authorId, tier)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiMineSubscription>> SubscriptionMineListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().MineList().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiUnsubscribeReply> SubscriptionUnsubscribeAsync(byte[] authorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Subscriptions().Unsubscribe(authorId).ConfigureAwait(false);
    }

#if PAYMENTS
    // Gated with its interface half — see INestRpcClient's payments region for why
    // (dynamic-features.md § Platform-family surface excision).
    // ── Payments — Pillar 3 client legs (monetization.md § Pillars 2+3) ──────
    // Thin FfiPaymentsClient pass-throughs — no client-side crypto, the nest
    // owns validation.

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiProviderItem>> PaymentsProvidersListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Payments().ProvidersList().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> PaymentsProvidersSetAsync(string kind, string webhookSecret, string tier)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Payments().ProvidersSet(kind, webhookSecret, tier).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> PaymentsProvidersRemoveAsync(string kind)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Payments().ProvidersRemove(kind).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiClaimRedeemReply> PaymentsClaimsRedeemAsync(string code)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Payments().ClaimsRedeem(code).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiClaimMintReply> PaymentsClaimsMintAsync(string tier, ulong? validUntil)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Payments().ClaimsMint(tier, validUntil).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<FfiClaimItem>> PaymentsClaimsListAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Payments().ClaimsList().ConfigureAwait(false);
    }

#endif   // PAYMENTS

    // ── Profile edit (display-name / bio / links read-modify-write) ──────
    // Thin FfiProfileClient pass-throughs over the session's connected requester;
    // the decode + sign+wire-build live in shared Rust (priority #2, no
    // client-side crypto). profile.md § State & data shape; linux edit.rs twin.

    /// <inheritdoc />
    public async Task<ProfileGetResult?> ProfileGetAsync(string actorId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        try
        {
            var body = await nest.Profile().ProfileGet(actorId).ConfigureAwait(false);
            var d = FaunaFfiMethods.DecodeProfileDisplay(body);
            var links = d.links
                .Select(l => new ProfileLinkRow(l.label, l.uri))
                .ToList();
            var display = new ProfileDisplay(d.displayName, d.bio, links);
            return new ProfileGetResult(display, body);
        }
        catch (FfiException)
        {
            // Unpublished profile (or a get/decode failure) — the first-publish
            // path. Mirrors linux edit.rs / refresh_header_name falling back to None.
            return null;
        }
    }

    /// <inheritdoc />
    public async Task<ProfileGetResult?> LoadProfileEditBaseAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        using var registry = RequireRegistry();
        try
        {
            var body = await nest.Profile()
                .LoadEditBase(_crypto.SecretBytes, registry)
                .ConfigureAwait(false);
            if (body is null)
                return null;
            var d = FaunaFfiMethods.DecodeProfileDisplay(body);
            var links = d.links
                .Select(l => new ProfileLinkRow(l.label, l.uri))
                .ToList();
            var display = new ProfileDisplay(d.displayName, d.bio, links);
            return new ProfileGetResult(display, body);
        }
        catch (FfiException)
        {
            // Read/decode failure — the first-publish path (mirrors ProfileGetAsync
            // above and apple's ProfileEditVM.open, which treats it the same way).
            return null;
        }
    }

    /// <inheritdoc />
    public async Task ProfileSetAsync(
        ProfileDisplay edited, byte[]? baseBody, FfiProfileImageEdit avatar, FfiProfileImageEdit banner)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var links = edited.Links
            .Select(l => new FfiProfileLink(l.Label, l.Uri))
            .ToArray();
        var body = FaunaFfiMethods.BuildEditedProfileWithImages(
            _crypto.SecretBytes, baseBody, ProfilePredecessors(), edited.DisplayName, edited.Bio,
            links, avatar, banner);
        await nest.Profile().ProfileSet(body).ConfigureAwait(false);
    }

    /// Whom this identity succeeded from, per the account registry — the only
    /// evidence that admits a stored profile signed by someone else as the base
    /// an edit re-signs (profile.md § After an identity succession, the
    /// successor RE-PUBLISHES). Empty for an identity that never succeeded, and
    /// in unit tests (<c>_accountRegistry</c> null).
    private string[] ProfilePredecessors()
    {
        try
        {
            using var registry = _accountRegistry?.Invoke();
            return registry?.PredecessorsOf(_crypto.ActorIdHex) ?? Array.Empty<string>();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient", $"[profile] predecessor resolve failed (empty fallback): {ex.Message}");
            return Array.Empty<string>();
        }
    }

    // ── Folder creation wizard ────────────────────────────────────────

    /// <inheritdoc />
    public async Task<FolderWizardMachine> BuildFolderWizardMachineAsync(
        FolderWizardObserver observer, IReadOnlyList<DeviceOption> availableDevices)
    {
        // Bind the machine to the session's connected WS-RPC requester; its
        // submit() issues fauna.folders.create + places.set over this same
        // FfiNestClient (the wrapper lives in libs/fauna-ffi/src/folders.rs).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildFolderWizardMachine(
            nest, observer, availableDevices.ToArray());
    }

    /// <inheritdoc />
    public async Task<DevicesMachine> BuildDevicesMachineAsync(DevicesObserver observer)
    {
        // Bind the page machine to the session's connected WS-RPC requester; its
        // reads + gestures + embedded wizard all ride this same FfiNestClient
        // (the wrapper lives in libs/fauna-ffi/src/devices.rs).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        // THE CONSUMER-WIRING RULE (path-sealing.md § Generalised): custody is
        // wired INSIDE BuildDevicesMachine, off the connection's own keypair —
        // deliberately unlike the retired separate WireDevicesLabelCustody call
        // this used to make. That second call was a plain Mutex REPLACE, not a
        // merge, so it silently downgraded the builder's full (resolver + owner)
        // custody back to owner-only — correct for device labels (always the
        // account's own plane) but wrong for the conflict list's shared/bound
        // folder paths, which need the resolver arm.
        var machine = FaunaFfiMethods.BuildDevicesMachine(nest, observer);

        // READ-side custody for a successor (succession-aftermath.md § Re-key
        // scope — the `BackupKey` corpus row): a succession re-points an owned
        // set and re-seals nothing, so an owner-only set's name still rests under
        // the predecessor's root, and without the paired chain the successor's
        // Folders page drops every set it inherited. It WIDENS the custody the
        // builder wired (read candidates only, never a seal root), so it comes
        // after the build. Skipped when empty. Mirrors tui's `label_custody` and
        // the Media build below.
        var (chainActorIds, chainKeys) = PredecessorChainOrEmpty("devices");
        if (chainActorIds.Length > 0)
        {
            machine.SetPredecessorChain(chainActorIds, chainKeys);
        }
        return machine;
    }

    /// <summary>
    /// The registry's paired predecessor chain for this session's actor — ids
    /// and keys, nearest hop first, the registry's own walk (never an app-side
    /// zip) — or two empty arrays when no registry is injected (unit tests), the
    /// identity never succeeded, or the read fails (logged under
    /// <paramref name="surface"/>). One read for every reader seam that takes the
    /// chain (Media, Devices), so they cannot drift.
    /// </summary>
    private (byte[][] actorIds, byte[][] keys) PredecessorChainOrEmpty(string surface)
    {
        try
        {
            var chain = _accountRegistry?.Invoke().PredecessorChain(_crypto.ActorIdHex);
            return (chain?.actorIds ?? Array.Empty<byte[]>(), chain?.keys ?? Array.Empty<byte[]>());
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient", $"[{surface}] predecessor-chain resolve failed (empty fallback): {ex.Message}");
            return (Array.Empty<byte[]>(), Array.Empty<byte[]>());
        }
    }

    /// <inheritdoc />
    public async Task<uniffi.fauna_backups_machine.IBackupsMachine> BuildBackupsMachineAsync(
        uniffi.fauna_backups_machine.BackupsObserver observer, string deviceIdHex)
    {
        // Bind the Backups snapshot-half machine to the session's connected WS-RPC
        // requester; its folder + snapshot reads, every write gesture and the
        // detail read all ride this same FfiNestClient (the wrapper lives in
        // libs/fauna-ffi/src/backups.rs).
        //
        // Custody is wired INSIDE build_backups_machine off the connection's own
        // keypair, the same pattern BuildDevicesMachineAsync now also relies on
        // (its own separate wire call was retired
        // for silently downgrading the builder's custody). It is why this page
        // cannot re-open bug (a sealed set's snapshot-detail-files
        // rendering EMPTY for a reader who holds the key). Nothing to wire here.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var machine = FaunaFfiMethods.BuildBackupsMachine(nest, observer);
        // Row provenance for manual creates (§ Snapshot-list shape, *Create*). The
        // setter itself drops an unparseable / wrong-length id rather than
        // half-setting it, so an empty device id simply leaves rows unattributed.
        if (!string.IsNullOrEmpty(deviceIdHex))
        {
            FaunaFfiMethods.BackupsMachineSetDeviceId(machine, deviceIdHex);
        }
        return machine;
    }

    /// <inheritdoc />
    public async Task WireDevicesForeignSetsAsync(DevicesMachine devices)
    {
        // Best-effort (folders.md § Implementation status today → Foreign-set
        // (cross-nest) list source): unreachable once this session is connected
        // (ConnectedAsync already required _crypto.SecretBytes to succeed), but
        // never surface a page error over a foreign-set row.
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            FaunaFfiMethods.WireDevicesForeignSets(devices, nest, _crypto.SecretBytes);
        }
        catch (FfiException)
        {
        }
    }

    /// <inheritdoc />
    public async Task<LabelerCatalogMachine> BuildLabelerCatalogMachineAsync(LabelerCatalogObserver observer)
    {
        // Bind the page-level LabelerCatalogMachine to the session's connected
        // WS-RPC requester; its fauna.labelers.* reads/gestures ride this same
        // FfiNestClient (the wrapper lives in libs/fauna-ffi/src/labeler_catalog.rs).
        // The actor secret gives it the grant seams: subscribing a wasm mail
        // labeler mints its per-labeler grant, unsubscribing revokes it.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildLabelerCatalogMachineWithGrants(nest, _crypto.SecretBytes, observer);
    }

    /// <inheritdoc />
    public async Task<MediaMachine> BuildMediaMachineAsync(MediaObserver observer)
    {
        // Bind the Media content-plane machine to the session's connected WS-RPC
        // requester; its cross-set fauna.media.list read rides this same
        // FfiNestClient (the wrapper lives in libs/fauna-ffi/src/media.rs).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var machine = FaunaFfiMethods.BuildMediaMachine(nest, observer);

        // READ-side custody for a successor (succession-aftermath.md § Re-key
        // scope — the `BackupKey` corpus row): without the retired owner's
        // BackupKeys offered here, a successor's inherited media renders as an
        // empty page indistinguishable from owning nothing. A second injection,
        // resolved off the same injected `_accountRegistry` the conversations-
        // session leg (above) uses, and deliberately never folded into
        // SetOwnerBackupKey — that one is the delete/restore *seal* root, and a
        // retired key must never reach it. Mirrors linux's build_media_view /
        // android's MediaVM.ensureMachine.
        byte[][] predecessorBackupKeys;
        try
        {
            predecessorBackupKeys = _accountRegistry?.Invoke().PredecessorBackupKeys(_crypto.ActorIdHex)
                ?? Array.Empty<byte[]>();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient", $"[media] predecessor-backup-keys resolve failed (empty fallback): {ex.Message}");
            predecessorBackupKeys = Array.Empty<byte[]>();
        }
        if (predecessorBackupKeys.Length > 0)
        {
            machine.SetPredecessorBackupKeys(predecessorBackupKeys);
        }

        // Ruling (8)(b)/(8)(c), writer-signed-change-records.md: the successor's
        // reader needs the account's attested predecessor ids (so a row signed
        // under a retired identity reads as the successor's own) and the PAIRED
        // id/key chain (a bare key above never opens a predecessor-signed row, so
        // without the pair the inherited corpus lists but cannot open). Both off
        // the registry — `PredecessorChain` is the registry's own walk
        // (`PredecessorChainOrEmpty`), never an app-side zip of the ids and keys.
        // Each skipped when empty, like the key injection. Mirrors tui's
        // `media::init` / linux's `build_media_view` / android's
        // MediaVM.ensureMachine.
        byte[][] predecessorActorIds;
        try
        {
            predecessorActorIds = _accountRegistry?.Invoke().AttestedPredecessorActorIds(_crypto.ActorIdHex)
                ?? Array.Empty<byte[]>();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRpcClient", $"[media] attested-predecessor-ids resolve failed (empty fallback): {ex.Message}");
            predecessorActorIds = Array.Empty<byte[]>();
        }
        var (chainActorIds, chainKeys) = PredecessorChainOrEmpty("media");
        if (predecessorActorIds.Length > 0)
        {
            machine.SetPredecessorActorIds(predecessorActorIds);
        }
        if (chainActorIds.Length > 0)
        {
            machine.SetPredecessorChain(chainActorIds, chainKeys);
        }

        // The share-link author (share-links.md § Where logic lives): the session's
        // identity signs the token and seals its filename, and the links point at
        // this session's nest. After the predecessors, which it reads — tui's
        // `media::init` / linux's `build_media_view` order. Unwired, every share
        // gesture reports its error rather than minting.
        machine.SetShareAuthor(_crypto.SecretBytes, _nestUrl);

        return machine;
    }

    /// <inheritdoc />
    public async Task WireMediaFollowedFoldersAsync(MediaMachine media)
    {
        // Best-effort, exactly like the Devices-page foreign-sets seam beside
        // it: unreachable once this session is connected (ConnectedAsync already
        // required _crypto.SecretBytes to succeed), and a failure here must
        // never surface as a page error — it degrades to "no followed scopes
        // offered", which is what an actor with no follows sees anyway.
        try
        {
            var nest = await ConnectedAsync().ConfigureAwait(false);
            FaunaFfiMethods.WireMediaFollowedFolders(media, nest, _crypto.SecretBytes);
        }
        catch (FfiException)
        {
        }
    }

    /// <inheritdoc />
    public async Task<MailPolicyMachine> BuildMailPolicyMachineAsync()
    {
        // Bind the admin-mail policy machine to the session's connected,
        // auto-reconnecting WS-RPC requester (same pattern as the devices
        // machine), rather than spinning up a per-page one-shot FfiNestClient.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildMailPolicyMachine(nest);
    }

    /// <inheritdoc />
    public async Task<CaldavPolicyMachine> BuildCaldavPolicyMachineAsync()
    {
        // Bind the admin-calendar CalDAV-enable machine to the session's connected,
        // auto-reconnecting WS-RPC requester (same pattern as the mail-policy
        // machine), rather than spinning up a per-page one-shot FfiNestClient.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildCaldavPolicyMachine(nest);
    }

    /// <inheritdoc />
    public async Task<CarddavPolicyMachine> BuildCarddavPolicyMachineAsync()
    {
        // Bind the admin-contacts CardDAV-enable machine to the session's
        // connected, auto-reconnecting WS-RPC requester (same pattern as the
        // CalDAV-policy machine), rather than spinning up a per-page one-shot
        // FfiNestClient.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildCarddavPolicyMachine(nest);
    }

    /// <inheritdoc />
    public async Task<WebdavPolicyMachine> BuildWebdavPolicyMachineAsync()
    {
        // Bind the admin-files WebDAV-enable machine to the session's connected,
        // auto-reconnecting WS-RPC requester (same pattern as the CardDAV-policy
        // machine), rather than spinning up a per-page one-shot FfiNestClient.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildWebdavPolicyMachine(nest);
    }

    /// <inheritdoc />
    public async Task<ForwarderMachine> BuildForwardersMachineAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildForwardersMachine(nest);
    }

    /// <inheritdoc />
    public async Task<BridgeApprovalMachine> BuildBridgeApprovalMachineAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildBridgeApprovalMachine(nest);
    }

    /// <inheritdoc />
    public async Task<DnsManagementMachine> BuildDnsManagementMachineWithCredentialsAsync()
    {
        // Credential-loading variant: the per-domain mode toggle's SetMode needs the
        // tip-sealed fauna.state.dns record (the account runtime); the keypair signs issuance.
        // The secret is supplied here (from the session crypto), never by the page.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildDnsManagementMachineWithCredentials(nest, _crypto.SecretBytes);
    }

    /// <inheritdoc />
    public async Task<LocalDomainMachine> BuildLocalDomainsMachineAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildLocalDomainsMachine(nest);
    }

    /// <inheritdoc />
    public async Task<LinkedNestsMachine> BuildLinkedNestsMachineAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildLinkedNestsMachine(nest);
    }

    /// <inheritdoc />
    public async Task<LinkedNestsMachine> BuildLinkedNestsMachineWithTrustAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildLinkedNestsMachineWithTrust(nest, _crypto.SecretBytes);
    }

    /// <inheritdoc />
    public async Task<LinkedNestsMachine> BuildLinkedNestsMachineWithMailRelayAsync()
    {
        // The user-facing Linked nests panel: build WITH the mail relay-provisioning
        // post-link hook (needs the actor secret, supplied here from the session crypto),
        // falling back to the hook-less build if keypair derivation throws — mirrors the
        // linux lead + the panel's prior inline try/catch (a both-ends LinkBoth then
        // auto-provisions the linked home box's mailbox; the hook no-ops when mail is off).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        try
        {
            return FaunaFfiMethods.BuildLinkedNestsMachineWithMailRelay(nest, _crypto.SecretBytes);
        }
        catch (Exception)
        {
            return FaunaFfiMethods.BuildLinkedNestsMachine(nest);
        }
    }

    /// <inheritdoc />
    public async Task<LinkedNestsMachine> BuildLinkedNestsMachineWithMailRelayAndTrustAsync()
    {
        // The user-facing Nests page: build WITH both the mail relay-provisioning
        // post-link hook AND the trust facet (both need the actor secret, supplied
        // here from the session crypto), falling back to the plain hook-less/
        // trust-less build if keypair derivation throws — mirrors the linux lead
        // (list/link/unlink still work; the hook is a no-op when mail is off, and
        // an untrusted-facet page still lists/links/unlinks).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        try
        {
            return FaunaFfiMethods.BuildLinkedNestsMachineWithMailRelayAndTrust(nest, _crypto.SecretBytes);
        }
        catch (Exception)
        {
            return FaunaFfiMethods.BuildLinkedNestsMachine(nest);
        }
    }

    /// <inheritdoc />
    public async Task<FfiWebClient> BuildWebClientAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildWebClient(nest);
    }

    /// <inheritdoc />
    public async Task<string> AdminFactoryResetAsync(string? newClaimCode)
    {
        // factory_reset stages the new claim code, replies, then exits + restarts
        // the nest; the reply carries the code (the human never sees it). A pinned
        // newClaimCode is honored verbatim — that is the CR-1 contract: the caller
        // has already persisted it, so a crash before the reply lands still leaves a
        // recoverable box. Rides the shared, auto-reconnecting client.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Admin().FactoryReset(newClaimCode).ConfigureAwait(false);
    }

    // ── Settings → Mail sub-page machines (shared fauna-client-mail-settings) ──

    /// <inheritdoc />
    public async Task<MailSettingsMachine> BuildMailSettingsMachineAsync()
    {
        // MailSettingsMachine needs the actor secret (sign submission tokens + key
        // the account plane) + node url (derive MUA details), supplied here from the session
        // crypto / nest url — see mail_admin.rs build_mail_settings_machine.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildMailSettingsMachine(nest, _crypto.SecretBytes, _nestUrl);
    }

    /// <inheritdoc />
    public async Task<MailSpamMachine> BuildMailSpamMachineAsync()
    {
        // Like BuildMailSettingsMachine: the machine's undo of a client-written
        // (sealed) history row runs the reseal loop, which derives the MSEK from
        // the actor secret + node url (mail_admin.rs build_mail_spam_machine).
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildMailSpamMachine(nest, _crypto.SecretBytes, _nestUrl);
    }

    /// <inheritdoc />
    public async Task<MailListsMachine> BuildMailListsMachineAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildMailListsMachine(nest);
    }

    /// <inheritdoc />
    public async Task<IAtprotoSettingsMachine> BuildAtprotoSettingsMachineAsync(AtprotoSettingsObserver observer)
    {
        // Same shape as BuildMailSettingsMachineAsync: the machine needs the actor
        // secret because the app-credential secrets it mints are sealed into the
        // actor's `fauna.state.atproto` store under the BackupKey derived from that seed — the nest
        // holds only the Argon2id verifier and can never recover them
        // (libs/fauna-ffi/src/atproto_settings.rs). Bound to the session's connected,
        // auto-reconnecting requester rather than a per-page one-shot client, the
        // same reuse-the-shared-connection pattern as every machine above.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildAtprotoSettingsMachine(nest, _crypto.SecretBytes, observer);
    }

    /// <inheritdoc />
    public async Task<IConnectedAppsMachine> BuildConnectedAppsMachineAsync(
        ConnectedAppsObserver observer, MailSettingsMachine? mail)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var machine = FaunaFfiMethods.BuildConnectedAppsMachine(nest, observer, mail);
        // Best-effort, like apple: without the account runtime an approve naming a
        // `fauna:records:` scope is refused by the machine, never resolved keyless.
        try { FaunaFfiMethods.WireConnectedAppsConsentGrant(machine, _crypto.SecretBytes); }
        catch (Exception) { /* a build without the account runtime refuses it */ }
        return machine;
    }

    /// <inheritdoc />
    public async Task<MailListMembersMachine> BuildMailListMembersMachineAsync(string listIdHex, string listName)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildMailListMembersMachine(nest, listIdHex, listName);
    }

    /// <inheritdoc />
    public async Task<MailExportMachine> BuildMailExportMachineAsync(string handle, string saveDir)
    {
        // WITH key custody, because MailExportViewModel spawns run_export after a
        // Start/Resume that lands Running — custody and the spawn are one change
        // (mail-export.md § Implementation status today). The secret opens every record
        // under the account's standing key set and wraps the per-session blob key
        // (§ Key material). No secret → the custody-less build, never custody without a
        // spawn: listing, Cancel and Discard keep working and Start refuses honestly.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        if (!_crypto.HasKey) return FaunaFfiMethods.BuildMailExportMachine(nest);
        return FaunaFfiMethods.BuildMailExportMachineWithKeyCustody(
            nest, _crypto.SecretBytes, _nestUrl, handle, saveDir);
    }

    /// <inheritdoc />
    public async Task<MailImportMachine> BuildMailImportMachineAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildMailImportMachine(nest);
    }

    /// <inheritdoc />
    public async Task<MailAliasesMachine> BuildMailAliasesMachineAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.BuildMailAliasesMachine(nest);
    }

    // ── CardDAV Address Book (contacts.md § Address Book segment; slice 4b) ──

    /// <inheritdoc />
    public async Task<FfiAddressbookRow[]> CarddavListAddressbooksAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Carddav().ListAddressbooks().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiCardRow[]> CarddavQueryCardsAsync(string addressbookIdHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Carddav().QueryCards(addressbookIdHex).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiLocatedCard> CarddavLocateCardByUidHashAsync(string uidHashHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await nest.Carddav().LocateCardByUidHash(uidHashHex).ConfigureAwait(false);
    }

    // ── Task delegation (participants.md § Task delegation) ────────────────

    /// <inheritdoc />
    public async Task<FfiTaskDelegationRow[]> TaskDelegationListAsync(string deviceId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var view = nest.TaskDelegationViewForDevice(
            Convert.FromHexString(deviceId), FfiHeavyTaskCapability.IndexOnly);
        return await view.Load().ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task TaskDelegationSetAssignmentAsync(string deviceId, string taskKind, FfiPinOption option)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        var view = nest.TaskDelegationViewForDevice(
            Convert.FromHexString(deviceId), FfiHeavyTaskCapability.IndexOnly);
        await view.SetAssignment(taskKind, option).ConfigureAwait(false);
    }

    // ── Recovery kit + the stolen-identity succession ──────────────────────
    //
    // Thin passes over `libs/fauna-ffi/src/recovery.rs`, the apple
    // `APIClient.swift` shape. Nothing here decides anything: the status line's
    // state, every action's enablement and the whole succession outcome arrive
    // already decided from the shared crate. In particular the succession is ONE
    // call and never its five steps — the ordering IS the correctness, and
    // exporting the steps would put it back in every app (recovery.rs § Why this
    // is ONE export and not five).

    /// A fresh registry view for one ceremony, or a loud refusal.
    ///
    /// <c>using</c>-scoped by every caller: the view is a cheap stateless face
    /// over the process-wide store, exactly as the account switcher opens one per
    /// switch. A missing factory is a wiring bug, not a runtime condition — and
    /// the three ceremonies that need it all write key material, so half-running
    /// one would mint a secret with nowhere to land.
    private FfiAccountRegistry RequireRegistry() =>
        _accountRegistry?.Invoke()
        ?? throw new InvalidOperationException(
            "no account registry wired into NestRpcClient — the recovery ceremonies "
            + "persist key material and cannot run without the credential store");

    /// <inheritdoc />
    public async Task<FfiRecoveryKitStatus> RecoveryKitStatusAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.RecoveryKitStatus(nest, _crypto.SecretBytes)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiMintedKit> RecoveryCreateKitAsync(string? heldKitInput)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        using var registry = RequireRegistry();
        return await FaunaFfiMethods
            .RecoveryCreateKit(nest, _crypto.SecretBytes, registry, heldKitInput)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiMintedKit> RecoveryRequestSeedAloneReplacementAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods
            .RecoveryRequestSeedAloneReplacement(nest, _crypto.SecretBytes)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> RecoveryVetoPendingReplacementAsync(string heldKitInput)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods
            .RecoveryVetoPendingReplacement(nest, _crypto.SecretBytes, heldKitInput)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<long> RecoveryResealEscrowWithHeldKitAsync(string heldKitInput)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        using var registry = RequireRegistry();
        return await FaunaFfiMethods
            .RecoveryResealEscrowWithHeldKit(nest, _crypto.SecretBytes, registry, heldKitInput)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiStolenOutcome> SuccessionSucceedWithHeldKitAsync(string kitInput)
    {
        // The CONNECTED client, deliberately — not a throwaway. The ceremony reads
        // the old identity's live MLS engine off this instance's own stashed
        // conversations session (`FfiNestClient::conversations_engine`), which is
        // the session `BuildConversationsSessionAsync` attached to exactly this
        // cached client. A fresh client would answer "no engine", and the sweep
        // would silently report NoEngine on a device whose groups it could have
        // re-pointed.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        using var registry = RequireRegistry();
        // A resolver, never a path: the ceremony invokes it only AFTER the
        // successor's connect, because resolving writes and an unreachable nest
        // must fail before anything is written (recovery.rs § The two things the
        // app still supplies).
        var storePath = new SuccessorStorePath();
        return await FaunaFfiMethods
            .SuccessionSucceedWithHeldKit(
                nest, _nestUrl, _crypto.SecretBytes, kitInput, registry, storePath)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiSweepRetryAnswer> SuccessionRetryGroupSweepAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        using var registry = RequireRegistry();
        // Two resolvers, deliberately different: the retired identity's is the
        // PURE scope read (RetiredIdentityStorePath — creates nothing, so the
        // retry's existence check answers honestly), the successor's is the
        // ordinary adopting one every other ceremony call uses.
        var oldStorePath = new RetiredIdentityStorePath();
        var successorStorePath = new SuccessorStorePath();
        return await FaunaFfiMethods
            .SuccessionRetryGroupSweep(
                nest, _crypto.SecretBytes, registry, oldStorePath, successorStorePath)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiOwedSweepAnswer> SuccessionDischargeOwedSweepAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        using var registry = RequireRegistry();
        // The retry's two resolvers, for the retry's reasons (above).
        var oldStorePath = new RetiredIdentityStorePath();
        var successorStorePath = new SuccessorStorePath();
        return await FaunaFfiMethods
            .SuccessionDischargeOwedSweep(
                nest, _crypto.SecretBytes, registry, oldStorePath, successorStorePath)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiAftermathOutcome> RunSuccessionAftermathAsync()
    {
        // The CONNECTED client, for the same reason the ceremony above takes one:
        // the pass's legs talk to this account's nest, and a throwaway would
        // re-handshake per call.
        var nest = await ConnectedAsync().ConfigureAwait(false);
        using var registry = RequireRegistry();
        return await FaunaFfiMethods
            .RunSuccessionAftermath(
                nest,
                _crypto.SecretBytes,
                // The app's data dir: the shared door no longer reads it (it
                // rooted the retired device-local `__config` replica; see
                // `docs/goal/architecture/config-dissolution.md`).
                BackupPaths.DataDir,
                registry,
                new Helpers.LoggingAftermathSink(this))
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<CustodyFacetView?> CustodyFacetLoadAsync()
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.CustodyFacetLoad(nest, _crypto.SecretBytes).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiCustodyActOutcome> CustodyRevokeAsync(byte[] grantId, byte[]? holder)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods
            .CustodyRevoke(nest, _crypto.SecretBytes, grantId, holder)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task CustodyDriveAsync(ConversationsSession session)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        // Returns once the pass is spawned — the export is async only so its
        // `tokio::spawn` has a runtime context (custody.rs `custody_drive`).
        await FaunaFfiMethods.CustodyDrive(nest, _crypto.SecretBytes, session).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiCustodyActOutcome> CustodyAcceptAsync(ConversationsSession session, byte[] grantId, bool onNest)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods
            .CustodyAccept(nest, _crypto.SecretBytes, session, grantId, onNest)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiCustodyActOutcome> CustodyDeclineAsync(byte[] grantId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.CustodyDecline(nest, _crypto.SecretBytes, grantId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiCustodyActOutcome> CustodySetBudgetAsync(byte[] grantId, ulong cap)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.CustodySetBudget(nest, _crypto.SecretBytes, grantId, cap).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiCustodyActOutcome> CustodyStopAsync(byte[] grantId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.CustodyStop(nest, _crypto.SecretBytes, grantId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<FfiCustodyActOutcome> CustodyRemoveAsync(byte[] grantId)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods.CustodyRemove(nest, _crypto.SecretBytes, grantId).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<bool> CustodyOfferShowsTargetSelectAsync(CustodyOfferRowView offer)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return FaunaFfiMethods.CustodyOfferShowsTargetSelect(nest, offer);
    }

    /// <inheritdoc />
    public IReadOnlyList<CustodyMintCandidateView> CustodyMintCandidates(ConversationsSession session)
        => FaunaFfiMethods.CustodyMintCandidates(_crypto.SecretBytes, session);

    /// <inheritdoc />
    public async Task<FfiCustodyActOutcome> CustodyMintAsync(ConversationsSession session, byte[] host, string channelHex)
    {
        var nest = await ConnectedAsync().ConfigureAwait(false);
        return await FaunaFfiMethods
            .CustodyMint(nest, _crypto.SecretBytes, session, host, channelHex)
            .ConfigureAwait(false);
    }

    /// <inheritdoc />
    public async Task<IReadOnlyList<bool>> DevicesKeylessPostureAsync(IReadOnlyList<string?> principals)
        => await FaunaFfiMethods.DevicesKeylessPosture(principals.ToArray()).ConfigureAwait(false);

    /// <summary>Raw device-id bytes → lowercase hex (the shape the old HTTP
    /// snapshot JSON carried), or null when unattributed.</summary>
    private static string? HexOrNull(byte[]? bytes) =>
        bytes is null ? null : FaunaFfiMethods.HexFull(bytes);

    public async ValueTask DisposeAsync()
    {
        _disposed = true;
        // Stop the author reconciliation poll loop — it has no watch stream to
        // end it when the session is torn down (sign-out / nest re-point).
        _authorPumpCts.Cancel();
        if (_nest is not null)
        {
            try { await _nest.Disconnect().ConfigureAwait(false); }
            catch { /* best-effort: tearing down the session regardless */ }
            _nest.Dispose();
            _nest = null;
        }
        _connectGate.Dispose();
    }
}
