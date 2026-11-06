using System;
using System.Threading.Tasks;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.Services;

/// <summary>
/// Holds the user-facing mail-import machine for the app session
/// (docs/goal/behavior/mailbox-migration.md § Wizard steps step 6: "leaving the page and
/// coming back keeps it"). The machine, not the page, owns the wizard's state — the
/// step, a finished session's counts and its skip log — so a panel built per visit over a
/// fresh machine would land every return on Source. tui keeps its machine for the same
/// reason; this is the session-scoped holder beside <see cref="NestRpcClient"/> that
/// gives windows the same shape. Keyed on the RPC client instance, so an account switch
/// (a new client) gets a new machine and never shows the previous account's import.
/// </summary>
internal static class MailImportMachineHolder
{
    private static INestRpcClient? _rpc;
    private static Task<MailImportMachine>? _machine;

    /// <summary>True while a panel's fetch-drive loop is running against the held
    /// machine. The loop outlives the panel that spawned it, so a panel built on return
    /// must not spawn a second <c>run_import</c> over the same machine.</summary>
    internal static bool ImportDriving { get; set; }

    /// <summary>The session's machine, built on first use over <paramref name="rpc"/>.</summary>
    internal static Task<MailImportMachine> GetAsync(INestRpcClient rpc)
    {
        if (!ReferenceEquals(_rpc, rpc) || _machine is null || _machine.IsFaulted)
        {
            _rpc = rpc;
            _machine = rpc.BuildMailImportMachineAsync();
            ImportDriving = false;
        }
        return _machine;
    }
}
