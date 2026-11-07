using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
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

    /// <summary>The account's external-apps switch (<c>atproto-external-apps-enable</c>) as the
    /// <c>state</c> attr the e2e reads. ON is the snapshot's zero value
    /// (<c>external_apps_off</c> false), so the switch reads <c>on</c> until a read says
    /// otherwise.</summary>
    public string ExternalAppsState => Snapshot?.@externalAppsOff == true ? "off" : "on";

    /// <summary>Flip the switch. Non-optimistic: the machine writes it and the page paints the
    /// re-read snapshot, never a local guess.</summary>
    public Task SetExternalAppsEnabledAsync(bool enabled) =>
        RunAsync(m => m.SetExternalAppsEnabled(enabled));

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
    // The roster's row text is a joined description as the item's own text (ui.yaml mints no
    // per-field leaves for the columns every row has). The lines are composed in shared Rust
    // (`compose::roster_row_text`); this view model only resolves and joins them.

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

    /// <summary>One roster row's joined description, composed once in shared Rust
    /// (<c>connected_apps_roster_row_text</c>): the name and class badge, the burned line, the
    /// publisher, one line per scope, the permission sets, the oracle summary, then the
    /// created / last-used / lasts-until facts as the last line.</summary>
    public static string RowText(ConnectedAppRow row)
    {
        var composed = FaunaFfiMethods.ConnectedAppsRosterRowText(row);
        var lines = composed.lines.Select(Strings.Resolve).ToList();
        lines.Add(string.Join(
            FaunaFfiMethods.ConnectedAppsFactsSeparator(),
            composed.facts.Select(Strings.Resolve)));
        return string.Join("\n", lines);
    }

    /// <summary>Whether the row links to its plugin's settings card on the Bridges page
    /// (<c>connected-apps-item-settings-link</c>, connected-apps.md § Layout &amp; flow region 3)
    /// — the shared predicate carried on the composed row (<c>opens_settings_card</c>), never
    /// a class test of this app's own.</summary>
    public static bool OpensSettingsCard(ConnectedAppRow row) =>
        FaunaFfiMethods.ConnectedAppsRosterRowText(row).opensSettingsCard;

    /// <summary>The attributes of a <c>connected-apps-item</c>, packed into the one HelpText UIA
    /// gives it (<see cref="AutomationAttributePack"/>): <c>revoked</c> — a mail app password or a
    /// signer client the identity-succession burn killed (an empty value is not a burned
    /// row), <c>status</c> — <c>paused</c> (the account's external-apps switch is OFF),
    /// <c>connected</c> or <c>not_connected</c>, as a value a reader can count beside the words —
    /// and, on a row a custodian served, <c>oracle_ops</c>, the operations in the current window.
    /// A row no custodian served leaves the key out, so the attribute reads as not set, never
    /// as zero.</summary>
    public static string RowAttributes(ConnectedAppRow row)
    {
        var burned = row.@mail is { @revoked: true } || row.@signerBurned;
        var attributes = new List<(string, string)>
        {
            ("revoked", burned ? "true" : ""),
            ("status", row.@paused ? "paused" : row.@connected ? "connected" : "not_connected"),
        };
        if (row.@oracle is { @opsInWindow: var ops }) attributes.Add(("oracle_ops", ops.ToString()));
        return AutomationAttributePack.Pack(attributes.ToArray());
    }

    /// <summary>One <c>connected-apps-request-card</c>'s own text, composed once in shared Rust
    /// (<c>connected_apps_request_card_lines</c>): the heading, who is asking (the resolved name
    /// plus the <c>client_id</c> verbatim), one line per requested scope, the permission-set
    /// provenance, the cli's reach line when the cli is asking, then what the approve ends. One
    /// card, a new start, never a second card (<c>connected-apps.md</c> § Architectural rules);
    /// nothing here derives a host or a fetch target from the client id, and there is
    /// deliberately no logo.</summary>
    public static string RequestText(ConsentCardRow request) =>
        string.Join("\n", FaunaFfiMethods.ConnectedAppsRequestCardLines(request).Select(Strings.Resolve));

    /// <summary>The install section of an admin's hosted-plugin install card — or a user's bind
    /// card — (<c>third-party.md</c> § The runner contract → <i>The install-approval leg</i>),
    /// each leaf's text resolved from the shared composition
    /// (<c>connected_apps_request_install_text</c>); <c>null</c> on every other card. The page
    /// paints it between the card text and the binding code, as tui's
    /// <c>install_section_elements</c> does.</summary>
    public static InstallSectionPaint? InstallSection(ConsentCardRow request)
    {
        if (FaunaFfiMethods.ConnectedAppsRequestInstallText(request) is not { } section) return null;
        var separator = FaunaFfiMethods.ConnectedAppsLineSeparator();
        string Many(uniffi.fauna_core.ComposedText[] lines) => string.Join(separator, lines.Select(Strings.Resolve));
        var form = request.install?.executionForm ?? string.Empty;
        return new InstallSectionPaint(
            Heading: Strings.Resolve(section.heading),
            Framing: section.framing is { } framing ? Strings.Resolve(framing) : null,
            Form: Strings.Resolve(section.form),
            FormValue: form,
            Publisher: Strings.Resolve(section.publisher),
            Digest: Strings.Resolve(section.digest),
            Kinds: Many(section.kinds),
            Hosts: Many(section.hosts),
            Settings: Many(section.settings));
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

/// <summary>One request card's install section, resolved for paint: one text per
/// <c>connected-apps-request-install*</c> leaf, plus the untagged framing line (an admin's
/// install card only) and the raw execution form the <c>-form</c> leaf carries as its
/// <c>form</c> attr.</summary>
internal sealed record InstallSectionPaint(
    string Heading,
    string? Framing,
    string Form,
    string FormValue,
    string Publisher,
    string Digest,
    string Kinds,
    string Hosts,
    string Settings);

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
