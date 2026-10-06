using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using FaunaApp.Core.Logs;

namespace FaunaApp.Services;

/// <summary>
/// The <b>running</b> instance's concurrent-instances affordance
/// (<c>docs/goal/architecture/apps/account-scoping.md</c> § Concurrent instances
/// → "the running instance's surface"): start a second app process bound to another
/// of this install's accounts, from the switcher row's
/// <c>account-open-new-instance-button</c>.
///
/// <para><b>The wiring channel is <c>FAUNA_BOUND_ACCOUNT=&lt;actor-id-hex&gt;</c> in
/// the child's environment</b> — one channel for all seven apps rather than a
/// per-platform argv/env/pipe choice, and deliberately the channel every app
/// already uses for launch wiring. It is bucket-1 IPC under the one-config-surface
/// invariant: the deployment artifact (here, the parent process) sets it, no human
/// ever types it.</para>
///
/// <para><b>Nothing is pre-checked here.</b> The child owns its whole binding
/// outcome — unknown account, re-auth-flagged account, already-served account are
/// all decided over there by <c>bind_account</c> plus the instance-lock acquire.
/// That is what keeps one gate instead of two that drift. The button is offered on
/// non-active rows only, and for a structural reason rather than tidiness: a spawn
/// for the account this instance already serves is always refused by the per-account
/// lock, so offering it would be offering a guaranteed failure.</para>
///
/// <para>The C# twin of linux's <c>instance_remote::spawn_bound_instance</c> and
/// apple's <c>InstanceSpawner</c>, including the <c>spawned_instances</c> state
/// records the e2e harness reads.</para>
/// </summary>
internal static class InstanceSpawner
{
    private const string LogSource = "InstanceSpawner";

    /// <summary>
    /// Mirrored from <c>fauna_client_accounts</c>, which exposes the <i>read</i>
    /// (<c>requested_bound_account</c>) rather than the constant — the same
    /// mirroring apple's and linux's spawners do.
    /// </summary>
    private const string BoundAccountEnv = "FAUNA_BOUND_ACCOUNT";

#if DEBUG || FAUNA_E2E_AGENT
    private const string BridgeEnv = "FAUNA_E2E_BRIDGE";
    private const string SessionEpochEnv = "FAUNA_E2E_SESSION_EPOCH";

    /// <summary>
    /// E2E only: the bridge a spawned child should report to. Windows' test agent is
    /// an outbound <i>poller</i> against a bridge URL, not a server on a port, so it
    /// has no analogue of linux's/apple's "give the child a free
    /// <c>FAUNA_E2E_AGENT_PORT</c>" — a child that merely inherited this process's
    /// bridge would <b>steal its commands and clobber its state pushes</b> (the
    /// bridge serves one agent per session epoch). So the parent's bridge is never
    /// inherited: a harness that wants to observe the child stands up a second
    /// bridge and names it here.
    /// </summary>
    private const string ChildBridgeEnv = "FAUNA_E2E_CHILD_BRIDGE";
#endif

    private static readonly object Gate = new();
    private static readonly List<(string ActorId, int Pid)> Spawned = new();

    /// <summary>
    /// Start a second instance bound to <paramref name="actorIdHex"/>. Returns the
    /// child's pid, or <c>null</c> if the process could not be started — the button
    /// is best-effort by design: the child, not the parent, reports a binding it
    /// cannot honour.
    /// </summary>
    public static int? Spawn(string actorIdHex)
    {
        if (string.IsNullOrWhiteSpace(actorIdHex))
        {
            return null;
        }

        var exe = Environment.ProcessPath;
        if (string.IsNullOrEmpty(exe))
        {
            ShellLog.Error(LogSource, "[instance-spawn] cannot resolve own executable");
            return null;
        }

        try
        {
            var psi = new ProcessStartInfo(exe)
            {
                // Required for Environment to be honoured at all, and it keeps the
                // child a direct child of this process rather than of the shell.
                UseShellExecute = false,
            };
            psi.Environment[BoundAccountEnv] = actorIdHex;

            // The rest of the environment is inherited wholesale on purpose: the
            // child must share this instance's install world (same credential store,
            // same data dir), which is the whole point of a second instance of the
            // same install. The two automation keys are the sole exception — see
            // ChildBridgeEnv.
            //
            // The whole re-keying block is compile-gated (convention 15): in a
            // shipped app none of these variables is ever set, so the block is a
            // no-op there and gating it keeps the three env NAMES out of the
            // Release IL along with the reads. Spawning itself is production
            // behaviour and stays ungated.
#if DEBUG || FAUNA_E2E_AGENT
            var childBridge = Environment.GetEnvironmentVariable(ChildBridgeEnv);
            if (!string.IsNullOrEmpty(childBridge))
            {
                psi.Environment[BridgeEnv] = childBridge;
                psi.Environment.Remove(SessionEpochEnv);
            }
            else if (Environment.GetEnvironmentVariable(BridgeEnv) is not null)
            {
                psi.Environment.Remove(BridgeEnv);
                psi.Environment.Remove(SessionEpochEnv);
            }
#endif

            var child = Process.Start(psi);
            if (child is null)
            {
                ShellLog.Error(LogSource, $"[instance-spawn] launch returned no process for {actorIdHex}");
                return null;
            }
            lock (Gate)
            {
                Spawned.Add((actorIdHex, child.Id));
            }
            ShellLog.Info(LogSource, $"[instance-spawn] {actorIdHex} → pid {child.Id}");
            return child.Id;
        }
        catch (Exception ex)
        {
            ShellLog.Error(LogSource, $"[instance-spawn] launch failed for {actorIdHex}: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Snapshot for the e2e state protocol's <c>spawned_instances</c> key —
    /// the cross-app shape (apple's <c>InstanceSpawner.stateRecords</c>, linux's
    /// <c>instance_remote::spawned_instances</c>), so one test reads every platform.
    /// </summary>
    public static IReadOnlyList<Dictionary<string, object?>> StateRecords()
    {
        lock (Gate)
        {
            return Spawned
                .Select(s => new Dictionary<string, object?>
                {
                    ["actor_id"] = s.ActorId,
                    ["pid"] = s.Pid,
                })
                .ToArray();
        }
    }
}
