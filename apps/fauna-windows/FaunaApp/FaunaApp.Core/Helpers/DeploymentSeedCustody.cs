using System;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// User-facing result of the deployment-seed custody leg. The shared leg
/// (<c>self_heal_deployment_seed_custody</c>) already does the work (the BR-2
/// mismatch refusal, the bounded fetch retry, the account-plane merge); this glue
/// only decides whether to raise a user-visible "recovery not protected" warning.
/// <see cref="Ok"/> shows nothing (custodied, not an admin, the expected multi-nest
/// no-op); the two <c>NotProtected*</c> cases warrant a warning banner.
/// </summary>
internal enum RecoveryCustodyOutcome
{
    /// <summary>Custodied, not owed, or the expected multi-nest no-op — no warning.</summary>
    Ok,
    /// <summary>BR-2: the box handed off a seed that does not derive to its pinned identity.</summary>
    NotProtectedMismatch,
    /// <summary>Custody is owed but is unconfirmed this connect (the fetch or the store failed, or the leg threw).</summary>
    NotProtectedFailed,
}

/// <summary>
/// Deployment-seed custody glue — the windows leg of box-recovery.md § The
/// plane-era recovery floor, <i>(c) The writes</i>. The custody leg is the ONLY
/// capture: the fauna-ffi account-runtime seat runs it at store-ready, and
/// <see cref="SelfHealAsync"/> is its post-auth entry, called on every connect
/// from <c>App.StartMainAppAsync</c>'s universal hook. There is no per-app retry,
/// BR-2 or claim-time latch logic here — this glue only maps the leg's outcome to
/// a user surface (the windows idiom of android's
/// <c>MailEnableGlueVM.recoveryCustodyOutcomeOf</c>) and reads the recovery box
/// list for the launch-retry entry.
/// </summary>
internal static class DeploymentSeedCustody
{
    /// <summary>
    /// Map the custody leg's outcome to the user-facing surface.
    /// <see cref="FfiDeploymentSeedSelfHeal.AlreadyCustodied"/> /
    /// <see cref="FfiDeploymentSeedSelfHeal.NotAdmin"/> and a captured
    /// <see cref="FfiDeploymentSeedCapture.Wrote"/> /
    /// <see cref="FfiDeploymentSeedCapture.AlreadyHeldSame"/> /
    /// <see cref="FfiDeploymentSeedCapture.RefusedDiffering"/> (the EXPECTED
    /// multi-nest no-op, BR-1: a log, never an alarm) are silent success;
    /// <see cref="FfiDeploymentSeedSelfHeal.HandoffUnavailable"/> /
    /// <see cref="FfiDeploymentSeedSelfHeal.NestHoldsNoSeed"/> are an unconfirmed
    /// custody ("recovery not protected", retried at the next connect); a captured
    /// <see cref="FfiDeploymentSeedCapture.RefusedMismatch"/> (BR-2 — the box
    /// handed off a seed that does not derive to its pinned identity) is the loud
    /// mismatch warning.
    /// </summary>
    internal static RecoveryCustodyOutcome MapOutcome(FfiDeploymentSeedSelfHeal outcome) => outcome switch
    {
        FfiDeploymentSeedSelfHeal.AlreadyCustodied
            or FfiDeploymentSeedSelfHeal.NotAdmin => RecoveryCustodyOutcome.Ok,
        FfiDeploymentSeedSelfHeal.HandoffUnavailable
            or FfiDeploymentSeedSelfHeal.NestHoldsNoSeed => RecoveryCustodyOutcome.NotProtectedFailed,
        FfiDeploymentSeedSelfHeal.Captured { v1: FfiDeploymentSeedCapture.RefusedMismatch } =>
            RecoveryCustodyOutcome.NotProtectedMismatch,
        _ => RecoveryCustodyOutcome.Ok,
    };

    /// <summary>
    /// Run the custody leg for THIS connection's nest — the post-auth entry
    /// (box-recovery.md § The plane-era recovery floor, <i>(c) The writes</i>) —
    /// and map the outcome. The leg is self-healing rather than event-driven: a
    /// live plane entry answers with no round trip, otherwise it checks roster
    /// membership, fetches the seed and merges it, so a claim, a co-admin granted
    /// later and a late-added device all converge the same way on their next
    /// connect. A thrown call is an unconfirmed custody
    /// (<see cref="RecoveryCustodyOutcome.NotProtectedFailed"/>), never a silent
    /// drop — android's precedent. Never throws.
    /// <para>No <c>ConfigureAwait(false)</c> — invoked from the UI-thread launch
    /// glue, continuations stay on the captured context per the WinUI
    /// off-thread-COMException rule.</para>
    /// </summary>
    internal static async Task<RecoveryCustodyOutcome> SelfHealAsync(INestRpcClient rpc)
    {
        try
        {
            var outcome = await rpc.SelfHealDeploymentSeedCustodyAsync();
            ShellLog.Info("DeploymentSeedCustody", $"custody leg: {outcome}");
            var mapped = MapOutcome(outcome);
            if (mapped == RecoveryCustodyOutcome.NotProtectedMismatch)
            {
                ShellLog.Error("DeploymentSeedCustody",
                    "deployment-seed custody refused: seed does not derive to this nest (BR-2); recovery not protected");
            }
            return mapped;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("DeploymentSeedCustody",
                $"custody leg failed, recovery not confirmed protected: {ex.GetType().Name}: {ex.Message}");
            return RecoveryCustodyOutcome.NotProtectedFailed;
        }
    }

    /// <summary>
    /// The recovery box list (box-recovery.md § The plane-era recovery floor,
    /// <i>(b) The reads</i>): <c>DeploymentSeeds(nest, …)</c> is already the shared
    /// local ⊔ cold join, so when the nest answers its result is the answer. When
    /// the nest cannot be reached (connect or read fails — a truly dead box) the
    /// read falls back to the device's own store
    /// (<c>DeploymentSeedsLocal</c>; desktop → no container argument), so a
    /// surviving device still lists every box it custodies. Never either-or on
    /// whether a nest URL is stored or answers. Hex <c>nest_actor_id</c> + domain
    /// only — the seed stays Rust-internal. Never throws: a local read that fails
    /// too is an empty array.
    /// </summary>
    internal static async Task<FfiDeploymentSeedEntry[]> ReadRecoverableBoxesAsync(
        string nestUrl, byte[] ownerSecret)
    {
        try
        {
            using var nest = new FfiNestClient(nestUrl, ownerSecret);
            await nest.Connect();
            return await FaunaFfiMethods.DeploymentSeeds(nest, ownerSecret);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("DeploymentSeedCustody",
                $"recovery box list: the nest did not answer, reading the device's own store: {ex.GetType().Name}: {ex.Message}");
        }
        return ReadLocalRecoverableBoxes(ownerSecret);
    }

    /// <summary>The device's own store read (<c>DeploymentSeedsLocal</c>), never throwing.</summary>
    internal static FfiDeploymentSeedEntry[] ReadLocalRecoverableBoxes(byte[] ownerSecret)
    {
        try
        {
            return FaunaFfiMethods.DeploymentSeedsLocal(ownerSecret, BackupPaths.DataDir);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("DeploymentSeedCustody",
                $"deployment_seeds_local failed, best-effort: {ex.GetType().Name}: {ex.Message}");
            return Array.Empty<FfiDeploymentSeedEntry>();
        }
    }

    /// <summary>
    /// The box-list read for the <c>launch-recover-button</c> surviving-device
    /// entry (box-recovery.md § Recovery UI (step 4)) — the windows twin of linux
    /// <c>client::load_recoverable_boxes</c> / web <c>loadRecoverableBoxes</c>.
    /// Called best-effort from <c>App.xaml.cs</c>'s transient-<c>Offline</c>
    /// launch-phase handler against the SAME saved <paramref name="nestUrl"/> the
    /// failed silent challenge targeted. A dead saved box no longer hides the
    /// button: <see cref="ReadRecoverableBoxesAsync"/> falls back to the device's
    /// own store. The one helper here that builds its own connection: it runs when
    /// the launch could NOT bring a session up, so there is no session connection
    /// to ride. Never throws.
    /// </summary>
    internal static async Task<string[]> LoadRecoverableBoxesAsync(string nestUrl, byte[] ownerSecret)
    {
        var boxes = await ReadRecoverableBoxesAsync(nestUrl, ownerSecret);
        return boxes.Select(b => b.nestActorId).ToArray();
    }
}
