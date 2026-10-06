using System.Threading;
using System.Threading.Tasks;
using uniffi.fauna_atproto_settings_machine;

namespace FaunaApp.Core.Services;

/// <summary>
/// Session-scoped memoized holder of the Bluesky/ATProto settings machine —
/// "build once, reuse across navigation", the fix the per-visit-rebuild bug
/// needs (mirrors android's <c>AtprotoSettingsHost</c> + its memoized
/// <c>ApiClient.buildAtprotoSettingsMachine</c> call).
/// <para>
/// Without this, a fresh <c>AtprotoPage</c>/<c>AtprotoViewModel</c> pair (page
/// caching stays off — the windows default, matching every other page) rebuilds
/// a fresh machine on every navigation to Settings → AT Protocol, silently
/// resetting the S4-C genesis-seniority custody check's one-convergence
/// debounce each time, so a real contradiction can never survive two
/// convergences through ordinary navigation — the SAME bug android hit and
/// fixed (`critical-alerts.md` § Implementation status today).
/// </para>
/// </summary>
internal sealed class AtprotoSettingsMachineHost
{
    public static readonly AtprotoSettingsMachineHost Instance = new();

    private readonly SemaphoreSlim _gate = new(1, 1);
    private IAtprotoSettingsMachine? _machine;

    private AtprotoSettingsMachineHost()
    {
    }

    /// <summary>
    /// The memoized machine, built on first call for this session. <paramref
    /// name="observer"/> is used only on the FIRST build: every page VM that
    /// consumes this host is non-optimistic (re-reads <c>Snapshot()</c> after
    /// each awaited gesture rather than repainting off a live callback — see
    /// <c>AtprotoViewModel</c>'s own doc comment), so a later caller's fresh
    /// no-op observer is never needed.
    /// </summary>
    public async Task<IAtprotoSettingsMachine> GetOrBuildAsync(
        INestRpcClient rpc, AtprotoSettingsObserver observer)
    {
        if (_machine is { } existing) return existing;
        await _gate.WaitAsync();
        try
        {
            _machine ??= await rpc.BuildAtprotoSettingsMachineAsync(observer);
            return _machine;
        }
        finally
        {
            _gate.Release();
        }
    }

    /// <summary>
    /// Drop the memoized machine — the identity-teardown boundary (sign-out,
    /// account switch, factory reset). The machine is bound to the secret it
    /// was built with, so reusing it across an account switch would mint/read
    /// ATProto settings under the WRONG actor.
    /// </summary>
    public void Reset() => _machine = null;
}
