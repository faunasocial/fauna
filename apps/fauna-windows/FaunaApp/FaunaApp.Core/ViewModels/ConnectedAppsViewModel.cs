using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_atproto_settings_machine;
using uniffi.fauna_client_connected_apps;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Settings → <b>Connected apps</b> page (<c>docs/goal/ui/connected-apps.md</c>;
/// ui.yaml page <c>connected-apps</c>, rail slot directly after Task delegation —
/// <c>settings.md</c> § Navigation model). A paint shell over the shared
/// <c>ConnectedAppsMachine</c> (<c>libs/fauna-client-connected-apps</c>, exported through
/// <c>fauna-ffi</c>): the roster's composition, the scope words, the class badge key,
/// <i>lasts-until</i> and <b>which verb revokes a row</b> are all the machine's. A row's
/// <c>key</c> is opaque here — this layer never picks a revoke verb. It owns the
/// machine, mirrors its snapshot, forwards each gesture, and holds only the page-local
/// state a gesture needs (the code field, the armed revoke, the revealed mail secrets).
/// Reference painters: tui <c>apps/fauna-tui/src/settings/connected_apps.rs</c>, and
/// FaunaKit's <c>ConnectedAppsVM</c> for the visit/handoff ordering.
///
/// <para><b>The mail app passwords are rows of this roster.</b> The machine takes the
/// session's Mail &amp; Calendar machine and reads, revokes and reveals them through it;
/// that machine's <c>credential_management_reachable</c> gate is only known after its own
/// <c>Hydrate()</c>, so <see cref="VisitAsync"/> hydrates a fresh one before it builds this
/// page's machine over it. A mail machine that fails to hydrate costs the roster its mail
/// rows, not the page.</para>
///
/// <para><b>A visit starts unread</b> (<c>connected-apps.md</c> § Errors &amp; edge cases):
/// rows are nest state read on every open, so a visit builds a fresh machine, drops the
/// previous visit's rows and paints neither rows nor the empty state until its own read has
/// returned, and takes every revealed secret off the screen.</para>
///
/// <para><b>Non-optimistic, like every sibling page VM:</b> each gesture awaits the machine
/// and re-reads <c>Snapshot()</c>, so the UI shows what the nest persisted. The observer is
/// therefore a no-op (the established windows pattern — the callback arrives on a Rust
/// thread with no dispatcher available in <c>FaunaApp.Core</c>).</para>
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> anywhere in this class — a WinUI VM that leaves the
/// UI context throws <c>COMException</c> on the next bound-property write.</para>
/// </summary>
internal partial class ConnectedAppsViewModel : ViewModelBase
{
    private readonly INestRpcClient? _rpc;
    private IConnectedAppsMachine? _machine;
    private string _handle;

    /// <summary>Which visit is current. A re-selected rail page (or a <c>fauna://consent</c>
    /// route landing on the page already on screen) restarts the visit, but an in-flight UniFFI
    /// await is not cancelled — so an older visit would otherwise keep writing the machine and
    /// snapshot a newer visit owns. Every await below re-checks.</summary>
    private int _visitGeneration;

    internal ConnectedAppsViewModel(INestRpcClient rpc, string handle)
    {
        _rpc = rpc;
        _handle = handle;
    }

    /// <summary>Test seam: drive the VM over a fake machine without a nest (machine-backed
    /// pages go over the uniffi <c>I&lt;Machine&gt;</c> interface so FaunaApp.Tests can fake
    /// it). <see cref="VisitAsync"/> then skips the build.</summary>
    internal ConnectedAppsViewModel(IConnectedAppsMachine machine, string handle)
    {
        _machine = machine;
        _handle = handle;
    }

    /// <summary>The last snapshot painted; null until the machine is built and read.</summary>
    public ConnectedAppsSnapshot? Snapshot { get; private set; }

    /// <summary>The <i>Connect an app</i> field's draft.</summary>
    public string Code { get; set; } = string.Empty;

    /// <summary>The row key whose inline revoke confirm is open.</summary>
    public string? RevokeArmed { get; private set; }

    /// <summary>The mail app-password secrets currently shown, by row key. Empty by default:
    /// the secret is never in the snapshot, so a row shows one only after the user asks and
    /// the on-demand read resolves. Keyed by row key, never by index, so a roster that
    /// re-orders under a fresh snapshot cannot show one password's secret against another
    /// row.</summary>
    public Dictionary<string, string> Revealed { get; } = new();

    /// <summary>One-time connect/build failure; page read/write failures live on the snapshot's
    /// <c>error</c>. Both reach the page's <c>error-message</c> through
    /// <see cref="PageError"/>.</summary>
    private string? _connectError;

    /// <summary>The page-level <c>error-message</c> text: the connect failure first, else the
    /// snapshot's localized <c>error</c>.</summary>
    public string? PageError =>
        _connectError ?? (Snapshot?.@error is { } e ? Strings.Resolve(e) : null);

    /// <summary>Whether the roster read has returned — the three-state list rule
    /// (<c>ui/README.md</c> § List pages): before it the page paints neither rows nor its empty
    /// state.</summary>
    public bool Loaded => Snapshot?.@loaded == true;

    /// <summary>Start a visit: drop the page-local drafts and the last visit's snapshot, build a
    /// fresh machine over a freshly hydrated mail machine, and read the roster. Then open the
    /// staged <c>fauna://consent</c> route, if any.</summary>
    public async Task VisitAsync()
    {
        _visitGeneration++;
        var generation = _visitGeneration;
        Code = string.Empty;
        RevokeArmed = null;
        Revealed.Clear();
        Snapshot = null;
        _connectError = null;

        if (_rpc is not null)
        {
            _machine = null;
            // A mail machine that fails to hydrate costs the roster its mail rows, not the
            // page — the machine records a real failure in its own snapshot.
            MailSettingsMachine? mail = null;
            try
            {
                mail = await _rpc.BuildMailSettingsMachineAsync();
                await mail.Hydrate();
            }
            catch (Exception)
            {
                // Mail rows are best-effort; `mail` stays whatever was built (or null).
            }
            if (generation != _visitGeneration) return;

            try
            {
                _machine = await _rpc.BuildConnectedAppsMachineAsync(new NoopObserver(), mail);
            }
            catch (Exception ex)
            {
                if (generation != _visitGeneration) return;
                _connectError = Strings.Error(ex);
                PublishChanged();
                // The staged route can never open without a machine: release its waiter so the
                // failure is read off the page's error-message, not after a timeout.
                if (PendingHandoff() is { } stuck) FinishHandoff(stuck);
                return;
            }
            if (generation != _visitGeneration) return;
        }

        var machine = _machine;
        if (machine is null) return;
        Snapshot = machine.Snapshot();
        await machine.Refresh();
        if (generation != _visitGeneration) return;
        Snapshot = machine.Snapshot();
        PublishChanged();
        await OpenStagedHandoffAsync();
    }

    /// <summary>Open the staged <c>fauna://consent/&lt;request_uri&gt;</c> route, if any, through
    /// the shared machine, and clear it only after the open has returned — a waiter (the e2e
    /// <c>open_route</c> command) then knows the card is painted. With nothing staged it is a
    /// no-op.</summary>
    public async Task OpenStagedHandoffAsync()
    {
        if (_machine is not { } machine || PendingHandoff() is not { } requestUri) return;
        var generation = _visitGeneration;
        await machine.OpenHandoff(requestUri);
        // A newer visit owns the page now: leave the request staged so ITS open (a fresh
        // machine) reveals the card and clears it.
        if (generation != _visitGeneration) return;
        Snapshot = machine.Snapshot();
        PublishChanged();
        FinishHandoff(requestUri);
    }

    /// <summary>The staged consent request URI, read through <see cref="HandoffSource"/>
    /// (production: <c>App.PendingConsentHandoff</c>).</summary>
    private string? PendingHandoff() => HandoffSource?.Pending;

    private void FinishHandoff(string requestUri) => HandoffSource?.Finish(requestUri);

    /// <summary>Where a staged route request lives and is cleared — the app's route door in
    /// production, a stub in unit tests. FaunaApp.Core cannot see <c>App</c>.</summary>
    public IConsentHandoffSource? HandoffSource { get; set; }

    // ── Gestures ──

    public async Task SubmitCodeAsync()
    {
        var typed = Code.Trim();
        if (typed.Length == 0) return;
        Code = string.Empty;
        await RunAsync(m => m.SubmitCode(typed));
    }

    public Task ResolveRequestAsync(string consentIdHex, bool approved) =>
        RunAsync(m => m.ResolveRequest(consentIdHex, approved));

    public Task BlockRequestAsync(string consentIdHex) => RunAsync(m => m.BlockRequest(consentIdHex));

    public Task UnblockAsync(string clientId) => RunAsync(m => m.Unblock(clientId));

    public void ArmRevoke(string key) => RevokeArmed = key;

    public void CancelRevoke() => RevokeArmed = null;

    public Task ConfirmRevokeAsync(string key)
    {
        RevokeArmed = null;
        Revealed.Remove(key);
        return RunAsync(m => m.Revoke(key));
    }

    /// <summary>Show the row's mail secret, or hide it if shown.</summary>
    public async Task ToggleRevealAsync(string key)
    {
        if (Revealed.Remove(key)) return;
        var secret = await ReadSecretAsync(key);
        if (secret is not null) Revealed[key] = secret;
    }

    /// <summary>Read a mail secret on demand for the clipboard, without painting it: Copy is
    /// independent of the reveal toggle, so the secret reaches the clipboard without being drawn
    /// on a screen someone else can read. A failed read leaves the machine's own error on the
    /// snapshot.</summary>
    public async Task<string?> ReadSecretAsync(string key)
    {
        if (_machine is not { } machine) return null;
        var secret = await machine.RevealSecret(key);
        Snapshot = machine.Snapshot();
        return secret;
    }

    /// <summary>The concrete login for a mail row, with only <c>{handle}</c> left for the shared
    /// <c>resolve_mua_username</c> to substitute (never a locally built address).</summary>
    public string MuaUsername(MailAppPassword mail) =>
        FaunaFfiMethods.ResolveMuaUsername(mail.@muaUsername, _handle);

    private async Task RunAsync(Func<IConnectedAppsMachine, Task> gesture)
    {
        if (_machine is not { } machine) return;
        await gesture(machine);
        Snapshot = machine.Snapshot();
    }

    /// <summary>Raised after a visit's reads land and after the staged open returns, so the page
    /// repaints. Gestures repaint through the page's own awaited calls.</summary>
    public event Action? Changed;

    private void PublishChanged() => Changed?.Invoke();

    /// <summary>No-op observer: this page re-reads the snapshot after every awaited gesture, so
    /// it needs no push reactivity (the established windows pattern).</summary>
    private sealed class NoopObserver : ConnectedAppsObserver
    {
        public void OnChanged()
        {
        }
    }

    // ── Row text composition ──
    //
    // The roster's row text is composed here (a joined description as the item's own text —
    // ui.yaml mints no per-field leaves for the columns every row has), the same shape tui and
    // linux paint. Lifting the composition into the shared crate is queued;
    // until it lands this is the one windows copy, and the page's `ui-actual-windows.yaml`
    // records it.

    /// <summary>The class badge's words — the one per-app half of the grouping key, which the
    /// machine derives. An unknown class paints no badge rather than a guess.</summary>
    public static string? ClassLabel(string cls) => cls switch
    {
        "remote" => Strings.Get("connected_apps/class_remote"),
        "device" => Strings.Get("connected_apps/class_device"),
        "wasm" => Strings.Get("connected_apps/class_wasm"),
        "container" => Strings.Get("connected_apps/class_container"),
        "app_password" => Strings.Get("connected_apps/class_app_password"),
        "signer" => Strings.Get("connected_apps/class_signer"),
        "oauth" => Strings.Get("connected_apps/class_oauth"),
        _ => null,
    };

    private static string When(long ms) => FaunaFfiMethods.FormatUnixLocalMs(ms);

    /// <summary>One roster row's joined description: the name and class badge, the burned line,
    /// the publisher, one line per scope, then the created / last-used / lasts-until facts.</summary>
    public static string RowText(ConnectedAppRow row)
    {
        var name = Strings.Resolve(row.@name);
        var head = ClassLabel(row.@class) is { } badge ? $"{name} · {badge}" : name;
        var lines = new List<string> { head };
        // The burned state sits directly under the name and above everything a user might copy
        // into a mail app: the login below it authenticates nothing any more
        // (mail-credentials.md § Rotation and recovery → Succession).
        if (row.@mail is { @revoked: true })
        {
            lines.Add(Strings.Get("settings/mail/credential_revoked"));
        }
        if (row.@clientId is { } clientId && row.@publisher is { } domain)
        {
            lines.Add($"{Strings.Get("connected_apps/publisher").Replace("{domain}", domain)} — {clientId}");
        }
        foreach (var scope in row.@scopeDescriptions)
        {
            lines.Add($"  • {Strings.Resolve(scope)}");
        }
        var facts = new List<string>();
        if (!row.@connected) facts.Add(Strings.Get("connected_apps/not_connected"));
        facts.Add(Strings.Get("connected_apps/created").Replace("{time}", When(row.@createdAtMillis)));
        facts.Add(row.@lastUsedAtMillis is { } used
            ? Strings.Get("connected_apps/last_used").Replace("{time}", When(used))
            : Strings.Get("connected_apps/never_used"));
        facts.Add(row.@lastsUntilMillis is { } until
            ? Strings.Get("connected_apps/lasts_until").Replace("{time}", When(until))
            : Strings.Get("connected_apps/open_ended"));
        lines.Add(string.Join(" · ", facts));
        return string.Join("\n", lines);
    }

    /// <summary>One <c>connected-apps-request-card</c>'s own text: the heading, who is asking
    /// (the resolved name plus the <c>client_id</c> verbatim), one line per requested scope and
    /// the permission-set provenance. The wording is the atproto page's
    /// <c>atproto_settings.consent_*</c> unchanged — one card, a new start, never a second card
    /// (<c>connected-apps.md</c> § Architectural rules). Nothing here derives a host or a fetch
    /// target from the client id, and there is deliberately no logo.</summary>
    public static string RequestText(ConsentCardRow request)
    {
        var who = request.@clientName is { } clientName
            ? Strings.Get("atproto_settings/consent_client")
                .Replace("{name}", clientName).Replace("{client_id}", request.@clientId)
            : Strings.Get("atproto_settings/consent_client_unnamed").Replace("{client_id}", request.@clientId);
        var asks = string.Join("\n", request.@scopeDescriptions.Select(line => $"  • {line}"));
        var sets = string.Concat(request.@sets.Select(set =>
        {
            var heading = set.@title is { } title
                ? Strings.Get("atproto_settings/consent_set_heading").Replace("{title}", title).Replace("{nsid}", set.@nsid)
                : Strings.Get("atproto_settings/consent_set_heading_unnamed").Replace("{nsid}", set.@nsid);
            var details = set.@details is { } d ? $"\n  {d}" : string.Empty;
            var members = string.Concat(set.@memberDescriptions.Select(line => $"\n  • {line}"));
            return $"\n{heading}{details}{members}";
        }));
        return $"{Strings.Get("atproto_settings/consent_heading")}\n{who}\n"
             + $"{Strings.Get("atproto_settings/consent_scopes_heading")}\n{asks}{sets}";
    }

    /// <summary>The binding code line (<c>connected-apps-request-code</c>'s text); the raw code is
    /// the element's <c>code</c> attr.</summary>
    public static string RequestCodeText(ConsentCardRow request) =>
        Strings.Get("atproto_settings/consent_code").Replace("{code}", request.@code);

    /// <summary>The blocked row's text: the client id verbatim, as the request card showed it,
    /// then when it was blocked. Nothing here parses the id into a host or a name.</summary>
    public static string BlockedText(BlockedAppRow blocked) =>
        $"{blocked.@clientId}\n{Strings.Get("connected_apps/blocked_since").Replace("{time}", When(blocked.@blockedAtMillis))}";
}

/// <summary>The staged <c>fauna://consent</c> request's home: the app's route door in
/// production. A seam only because <c>FaunaApp.Core</c> cannot see <c>App</c>.</summary>
internal interface IConsentHandoffSource
{
    /// <summary>The request URI the route asked the page to open, or null.</summary>
    string? Pending { get; }

    /// <summary>Called after the machine's open returned: clear the stage and release the
    /// waiter.</summary>
    void Finish(string requestUri);
}
