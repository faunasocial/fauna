using System;
using System.Collections.Generic;
using System.Threading.Tasks;

namespace FaunaApp.Core.Services;

/// <summary>
/// The onboarding wizard's sign-in follow-ups, handed to the session that the
/// landing starts: sealing the captured DNS credential, the one-tap trust mint,
/// and registering the confirmed recovery kit. Each is best-effort, and each
/// needs an authenticated connection, which exists only once the landing's
/// universal post-auth hook (<c>App.StartMainAppAsync</c>) has built the
/// session's client. So the wizard queues them here, keyed by the actor they
/// belong to, and the hook takes and runs them on that session's ONE client
/// (<c>transport.md</c>: one authenticated WebSocket per actor).
///
/// <para>Until 2026-09-28 each follow-up built a one-shot <c>FfiNestClient</c> of
/// its own, so a first sign-in dialled a socket per follow-up on top of the
/// session's own, all from the process's per-nest dial budget
/// (<c>transport-connection.md</c> § The dial budget). linux and tui already run
/// the same follow-ups on their session client at the sign-in hand-off.</para>
///
/// <para>The actor key is the guard: a follow-up captured for one identity never
/// runs under another account activated afterwards (the rule
/// <see cref="SuccessionHandoff.ClaimOwedKit"/> keeps too).</para>
/// </summary>
internal static class PostSignInHandoff
{
    private static readonly object Gate = new();
    private static readonly List<(string Actor, string What, Func<INestRpcClient, Task> Run)> Pending = new();

    /// <summary>Queue <paramref name="run"/> for <paramref name="actorIdHex"/>'s
    /// session. <paramref name="what"/> names it in the log.</summary>
    internal static void Enqueue(string actorIdHex, string what, Func<INestRpcClient, Task> run)
    {
        lock (Gate) Pending.Add((actorIdHex, what, run));
    }

    /// <summary>Take every follow-up queued for <paramref name="actorIdHex"/>, in
    /// order, removing them. Others' stay queued.</summary>
    internal static IReadOnlyList<(string What, Func<INestRpcClient, Task> Run)> TakeFor(string? actorIdHex)
    {
        var taken = new List<(string, Func<INestRpcClient, Task>)>();
        if (string.IsNullOrEmpty(actorIdHex)) return taken;
        lock (Gate)
        {
            Pending.RemoveAll(entry =>
            {
                if (!string.Equals(entry.Actor, actorIdHex, StringComparison.OrdinalIgnoreCase)) return false;
                taken.Add((entry.What, entry.Run));
                return true;
            });
        }
        return taken;
    }

    /// <summary>Run every follow-up queued for <paramref name="actorIdHex"/> on
    /// <paramref name="rpc"/>, one after another. Best-effort: a failure is
    /// logged and the next one still runs. Never throws.</summary>
    internal static async Task RunForAsync(string? actorIdHex, INestRpcClient rpc)
    {
        foreach (var (what, run) in TakeFor(actorIdHex))
        {
            try
            {
                await run(rpc);
            }
            catch (Exception ex)
            {
                Logs.ShellLog.Error("PostSignInHandoff",
                    $"[onboarding] {what} failed: {ex.GetType().Name}: {ex.Message}");
            }
        }
    }

    /// <summary>Drop every queued follow-up — the credential-namespace wipe
    /// (sign-out, factory reset) leaves no identity for them to run as.</summary>
    internal static void Clear()
    {
        lock (Gate) Pending.Clear();
    }
}
