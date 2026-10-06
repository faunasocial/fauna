using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Admin host-address reporting — the windows leg of
/// <c>clients-host-address-onboarding</c>. On connect the admin client reports
/// the nest's PUBLIC IP (<c>fauna.dns.set_host_address</c>) so ACME HTTP-01
/// gates on the STRONG resolve-check
/// (<c>domains-and-tls-bootstrap.md</c> § Host-address acquisition). The native
/// twin of linux's <c>AdminStatusLoaded → report_host_address()</c> and web's
/// <c>+layout.svelte</c> <c>reportHostAddress</c>.
/// <para>
/// Admin-gated (<c>am_i_admin</c>): a non-admin's <c>set_host_address</c> is
/// refused nest-side, so gating avoids a pointless failing RPC on every
/// non-admin connect. The classify + never-publish-a-private-address safety +
/// the whole decision tree live in the shared FFI fn — there is NO client
/// logic here (priority #2). Fire-and-forget + idempotent (last-writer-wins on
/// the nest, so repeat connects converge an IP change); best-effort — a fault
/// is logged and must never disrupt login (the reconnect/knock-pump stance).
/// </para>
/// </summary>
internal sealed class HostAddressReporter
{
    private readonly INestRpcClient _rpc;

    public HostAddressReporter(INestRpcClient rpc) => _rpc = rpc;

    /// <summary>Report the public host-address once iff this identity is the
    /// nest admin. Never throws.</summary>
    public async Task RunAsync()
    {
        try
        {
            // Gate: only the admin reports (a non-admin's set_host_address is
            // refused nest-side → a pointless Failed each connect). Mirrors
            // linux's `if is_admin { report_host_address() }`.
            if (!await _rpc.AmIAdminAsync().ConfigureAwait(false)) return;

            var outcome = await _rpc.ReportHostAddressAsync().ConfigureAwait(false);
            switch (outcome)
            {
                case FfiHostAddressOutcome.Reported r:
                    ShellLog.Info("HostAddressReporter",
                        $"reported nest public IPv4 {r.@nestIpv4} (strong ACME resolve-gate now in force)");
                    break;
                case FfiHostAddressOutcome.SkippedNoPublicIp:
                    // Expected on a home-LAN box (no reflector) — the spec'd safe
                    // floor, not an alarm. domains-and-tls-bootstrap.md § "No
                    // public IP determinable".
                    ShellLog.Debug("HostAddressReporter",
                        "no public IP determinable — kept self-signed floor (LAN box)");
                    break;
                case FfiHostAddressOutcome.Failed f:
                    ShellLog.Warn("HostAddressReporter",
                        $"set_host_address failed (retries next connect): {f.@error}");
                    break;
            }
        }
        catch (Exception ex)
        {
            // Best-effort — a fault must never disrupt login (the reconnect/knock
            // pump stance). The next connect retries idempotently.
            ShellLog.Warn("HostAddressReporter",
                $"host-address report skipped: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
