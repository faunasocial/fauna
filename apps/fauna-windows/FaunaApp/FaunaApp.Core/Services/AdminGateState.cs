using FaunaApp.Core.Logs;

namespace FaunaApp.Core.Services;

/// <summary>
/// The admin gate (<c>fauna.account.am_i_admin</c>) read at login AND on every
/// reconnect (transport.md § Which surfaces a push invalidates, the
/// <c>admin_gate</c> flag). Two properties, as on tui and linux:
/// (1) a FAILED read moves nothing — the gate keeps its last answer, so a
/// confirmed admin's <c>admin-tab</c> survives a transient fault, and a login
/// read that fails before the nest's name resolves is repaired by the next
/// reconnect; (2) the once-per-session admin effects fire on the
/// false→true transition only (<see cref="AdminGateRefresh.BecameAdmin"/>),
/// never on every reconnect. One instance per <c>MainPage</c> (so an account
/// switch starts from "not an admin").
/// </summary>
internal sealed class AdminGateState
{
    /// <summary>The last answer a successful read gave (false until one does).</summary>
    public bool IsAdmin { get; private set; }

    /// <summary>Re-read the gate. Never throws.</summary>
    public async Task<AdminGateRefresh> RefreshAsync(INestRpcClient rpc)
    {
        bool read;
        try
        {
            read = await rpc.AmIAdminAsync();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("AdminGateState", $"[admin-gate] am_i_admin read failed: {ex.Message}");
            return AdminGateRefresh.Unreachable;
        }

        ShellLog.Info("AdminGateState", $"[admin-gate] am_i_admin={read}");
        var became = read && !IsAdmin;
        IsAdmin = read;
        return became ? AdminGateRefresh.BecameAdmin : AdminGateRefresh.Unchanged;
    }
}

internal enum AdminGateRefresh
{
    /// <summary>The read failed; the gate kept its last answer.</summary>
    Unreachable,
    /// <summary>The read succeeded and the answer did not flip to admin.</summary>
    Unchanged,
    /// <summary>The read succeeded and the gate went false→true.</summary>
    BecameAdmin,
}
