using System;
using System.Collections.Generic;
using System.Linq;
using System.Net;
using System.Net.Sockets;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;
using uniffi.fauna_onboarding_machine;

namespace FaunaApp.Core.ViewModels;

/// <summary>Which surface opened the <c>nest_retire</c> page
/// (<c>nest-retirement.md</c> § Layout &amp; flow).</summary>
internal enum RetireEntry
{
    /// <summary><c>launch-retire-button</c> on the launch retry surface — no session.</summary>
    Launch,
    /// <summary><c>admin-nest-retire-button</c> over a live admin session.</summary>
    Admin,
}

/// <summary>What the page's machine is built from — the constructor inputs
/// <c>NestRetireMachine::new</c> takes that this app knows at open time.</summary>
internal sealed record RetireMachineInputs(
    string[] CandidateDomains, string? VpsBaseUrl, string? DohBaseUrl);

/// <summary>
/// Hosts the <c>nest_retire</c> page over the shared <see cref="NestRetireMachine"/>
/// (<c>docs/goal/behavior/nest-retirement.md</c> § Where logic lives): this
/// view-model only builds the machine from what the app knows, forwards gestures
/// and announces a repaint — no page logic and no provider branch (priority #2).
/// The windows lift of FaunaKit's <c>NestRetireVM</c>, android's
/// <c>NestRetireScreen</c> and tui's <c>wizard/nest_retire.rs</c>.
///
/// <para>One machine per page visit: <see cref="Open"/> builds it,
/// <see cref="Leave"/> drops it. The VPS token typed into it is never
/// persisted. The held DNS credentials and the remembered DNS slot names come
/// from ONE shared read, <c>fauna_ffi::retire_held_inputs</c> — never a windows
/// read of <c>fauna.state.dns</c> of its own (§ Implementation status today).</para>
///
/// <para>The machine's async calls move its snapshot while they run, so the page
/// repaints on a timer while it is up as well as on <see cref="Changed"/>.</para>
/// </summary>
internal sealed class NestRetireViewModel
{
    /// <summary>How long resolving the saved nest's host for the <i>current</i>
    /// badge may take.</summary>
    internal static readonly TimeSpan ResolveBound = TimeSpan.FromSeconds(5);

    private readonly Func<RetireMachineInputs, INestRetireMachine> _buildMachine;
    private readonly Func<bool, Task<FfiRetireHeldInputs>> _readHeldInputs;
    private readonly Func<string, Task<string?>> _resolveIpv4;
    private readonly Dictionary<string, string> _creds = new();

    public INestRetireMachine? Machine { get; private set; }
    public RetireEntry Entry { get; private set; } = RetireEntry.Launch;

    /// <summary>What the person typed into the confirm field, as painted.</summary>
    public string ConfirmTyped { get; private set; } = "";

    /// <summary>Raised after every machine call and every input that landed — the
    /// page re-reads the snapshot. May fire off the UI thread; the page marshals.</summary>
    public event Action? Changed;

    /// <param name="buildMachine">Builds the page's machine; the default is the
    /// real <see cref="NestRetireMachine"/>. Tests pass a fake.</param>
    /// <param name="readHeldInputs">The shared held-DNS read; the default is
    /// <c>FaunaFfiMethods.RetireHeldInputs</c>.</param>
    /// <param name="resolveIpv4">Resolves a host to its IPv4; the default is the
    /// OS resolver.</param>
    public NestRetireViewModel(
        Func<RetireMachineInputs, INestRetireMachine>? buildMachine = null,
        Func<bool, Task<FfiRetireHeldInputs>>? readHeldInputs = null,
        Func<string, Task<string?>>? resolveIpv4 = null)
    {
        _buildMachine = buildMachine ?? (i => new NestRetireMachine(
            i.CandidateDomains, null, Array.Empty<string>(), Array.Empty<string>(),
            i.VpsBaseUrl, i.DohBaseUrl));
        _readHeldInputs = readHeldInputs ?? FaunaFfiMethods.RetireHeldInputs;
        _resolveIpv4 = resolveIpv4 ?? ResolveIpv4Async;
    }

    private void RaiseChanged() => Changed?.Invoke();

    /// <summary>
    /// Open the page. The machine takes the e2e provider base-URL overrides at
    /// construction (<c>null</c> in every release build), the saved nest's host
    /// as an attribution candidate, and — once resolved — that host's IPv4 for
    /// the <i>current</i> badge. The rest of what the app knows lands
    /// asynchronously (<see cref="ReadInputsAsync"/>): every held DNS credential
    /// and the remembered DNS slots through the shared read, and, from
    /// <c>admin-nest</c>, the nest's active local domains (§ Credential stance (b),
    /// § DNS cleanup → <i>Several domains</i> (1)). Returns the task that lands
    /// those inputs (the page ignores it; tests await it).
    /// </summary>
    public Task Open(
        RetireEntry entry,
        string? nestUrl,
        string? vpsBaseUrl,
        string? dohBaseUrl,
        Func<Task<IReadOnlyList<string>>> activeLocalDomains)
    {
        Finish();
        Entry = entry;
        var host = HostOf(nestUrl);
        var candidates = host is not null && !IsIpLiteral(host)
            ? new[] { host }
            : Array.Empty<string>();
        var m = _buildMachine(new RetireMachineInputs(candidates, vpsBaseUrl, dohBaseUrl));
        Machine = m;
        // Verify is where the machine reads the inputs, so it waits for them.
        m.ExpectAppInputs();
        var inputs = ReadInputsAsync(m, entry, activeLocalDomains);
        if (host is not null) _ = ResolveCurrentAsync(m, host);
        RaiseChanged();
        return inputs;
    }

    private async Task ReadInputsAsync(
        INestRetireMachine m, RetireEntry entry, Func<Task<IReadOnlyList<string>>> activeLocalDomains)
    {
        try
        {
            if (entry == RetireEntry.Admin)
            {
                var domains = await activeLocalDomains();
                if (domains.Count > 0) m.AddCandidateDomains(domains.ToArray());
            }
            // The shared read: `fauna.state.dns` through the account runtime,
            // bounded in Rust. The admin entry waits for a runtime still
            // assembling; the launch entry (no session) reads only one already up.
            var held = await _readHeldInputs(entry == RetireEntry.Admin);
            m.SetHeldDnsJson(held.@heldDnsProviderIds, held.@heldDnsCredsJson);
            m.SetRememberedSlotsJson(
                held.@rememberedDkimJson, held.@rememberedAtprotoJson, held.@rememberedFaunaSelfJson);
            // A first-listing refusal is not "the account holds none": say why.
            if (held.@notReadyReason is { } reason) m.SetInputError(reason);
        }
        catch (Exception ex)
        {
            // Best-effort inputs: the page goes on with what it opened with.
            ShellLog.Warn("NestRetire", $"[retire] app inputs read failed: {ex.GetType().Name}: {ex.Message}");
        }
        finally
        {
            m.AppInputsLanded();
            RaiseChanged();
        }
    }

    private async Task ResolveCurrentAsync(INestRetireMachine m, string host)
    {
        string? ipv4 = null;
        try
        {
            var lookup = _resolveIpv4(host);
            if (await Task.WhenAny(lookup, Task.Delay(ResolveBound)) == lookup) ipv4 = await lookup;
        }
        catch (Exception ex)
        {
            ShellLog.Info("NestRetire", $"[retire] resolving the saved nest's host failed: {ex.GetType().Name}");
        }
        if (ipv4 is not null && ReferenceEquals(Machine, m))
        {
            m.SetCurrentIpv4(ipv4);
            RaiseChanged();
        }
    }

    /// <summary>The host of <paramref name="nestUrl"/>, or <c>null</c>.</summary>
    internal static string? HostOf(string? nestUrl)
        => Uri.TryCreate(nestUrl ?? "", UriKind.Absolute, out var uri) && uri.Host.Length > 0
            ? uri.Host.Trim('[', ']')
            : null;

    /// <summary>An IP literal is never a domain to attribute.</summary>
    internal static bool IsIpLiteral(string host) => IPAddress.TryParse(host, out _);

    private static async Task<string?> ResolveIpv4Async(string host)
    {
        if (IPAddress.TryParse(host, out var literal))
            return literal.AddressFamily == AddressFamily.InterNetwork ? literal.ToString() : null;
        var addrs = await Dns.GetHostAddressesAsync(host, AddressFamily.InterNetwork);
        return addrs.FirstOrDefault()?.ToString();
    }

    /// <summary>Run a machine call, announcing a repaint when it returns.</summary>
    private async void RunOp(Func<INestRetireMachine, Task> op, string what)
    {
        if (Machine is not { } m) return;
        try { await op(m); }
        catch (Exception ex)
        {
            ShellLog.Warn("NestRetire", $"[retire] {what} threw: {ex.GetType().Name}: {ex.Message}");
        }
        RaiseChanged();
    }

    // ── credentials ──────────────────────────────────────────────────────

    public void SelectProvider(string providerId)
    {
        if (Machine is not { } m) return;
        if (m.SelectedProvider() != providerId) _creds.Clear();
        m.SelectProvider(providerId);
        RaiseChanged();
    }

    public void SetCred(string fieldId, string value)
    {
        Machine?.SetCredentialField(fieldId, value);
        _creds[fieldId] = value;
        RaiseChanged();
    }

    /// <summary>The typed credential field as painted (the machine holds it secret).</summary>
    public string GetCred(string fieldId) => _creds.TryGetValue(fieldId, out var v) ? v : "";

    public void Verify() => RunOp(m => m.Verify(), "verify");

    /// <summary>The <c>hosted-auth</c> sign-in as ONE call — begin, hand the
    /// verification URL to <paramref name="openUrl"/>, poll until the token lands
    /// — the same sequencing <see cref="OnboardingViewModel"/>'s own runs, kept out
    /// of the view (its doc says why).</summary>
    public async Task HostedAuthRun(string fieldId, Action<string> openUrl)
    {
        if (Machine is not { } m) return;
        var prompt = await m.HostedAuthBegin(fieldId);
        RaiseChanged();
        if (prompt is null) return; // the field is Failed; the button repaints from it
        try { openUrl(prompt.@verificationUrl); }
        catch { /* no browser association is not a reason to abandon the poll */ }
        await m.HostedAuthWait(fieldId);
        RaiseChanged();
    }

    // ── list / confirm ───────────────────────────────────────────────────

    public void Select(string serverId)
    {
        Machine?.Select(serverId);
        RaiseChanged();
    }

    public void FetchTransferCode() => RunOp(m => m.FetchTransferCode(), "fetch_transfer_code");

    public void BeginConfirm()
    {
        ConfirmTyped = "";
        Machine?.BeginConfirm();
        RaiseChanged();
    }

    public void SetConfirmName(string typed)
    {
        Machine?.SetConfirmName(typed);
        ConfirmTyped = typed;
        RaiseChanged();
    }

    public void CancelConfirm()
    {
        ConfirmTyped = "";
        Machine?.Back();
        RaiseChanged();
    }

    public void Confirm() => RunOp(m => m.Confirm(), "confirm");
    public void Retry() => RunOp(m => m.Retry(), "retry");
    public void ForceServer() => RunOp(m => m.ForceServer(), "force_server");

    /// <summary>
    /// Back, as tui settled it: from credentials — and from a run stopped on a
    /// failure (disarming it) — leave the page; from list / confirm one state
    /// back; never while a step is in flight. Returns whether the page is left.
    /// </summary>
    public bool Back()
    {
        if (Machine is not { } m) return true;
        switch (m.Snapshot().@phase)
        {
            case RetirePhase.Credentials:
            case RetirePhase.Running:
            case RetirePhase.Done:
                return true;
            case RetirePhase.List:
            case RetirePhase.Confirm:
                ConfirmTyped = "";
                m.Back();
                RaiseChanged();
                return false;
            default:
                return false;
        }
    }

    /// <summary>
    /// Close the page: drop the token and everything fetched with it. Returns
    /// whether this session's own nest was retired from <c>admin-nest</c> — the
    /// session this device holds is then for a nest that no longer exists, so the
    /// host ends it keeping credentials and re-enters launch routing
    /// (§ Where logic lives, ruled 2026-11-01).
    /// </summary>
    public bool Leave()
    {
        var retiredCurrent = Machine?.Snapshot().@retired?.@current == true;
        var dropSession = retiredCurrent && Entry == RetireEntry.Admin;
        Finish();
        return dropSession;
    }

    private void Finish()
    {
        Machine?.Cancel();
        Machine = null;
        _creds.Clear();
        ConfirmTyped = "";
    }
}
