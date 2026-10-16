using System;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Deployment-seed custody glue — the windows leg of box-recovery.md § The
/// plane-era recovery floor, <i>(c) The writes</i>. The custody leg is the ONLY
/// capture: the fauna-ffi account-runtime seat runs it at store-ready, and
/// <see cref="SelfHealAsync"/> is its post-auth entry, called on every connect
/// from <c>App.StartMainAppAsync</c>'s universal hook. There is no per-app retry,
/// BR-2, claim-time latch or warning-selection logic here — the leg answers the
/// warning it owes as the shared <c>LocalizedText</c>
/// (<c>fauna_client_config::custody_leg_warning</c>), which the caller renders
/// through <c>Strings.Resolve</c> — and this glue reads the recovery box list for
/// the launch-retry entry.
/// </summary>
internal static class DeploymentSeedCustody
{
    /// <summary>
    /// Run the custody leg for THIS connection's nest — the post-auth entry
    /// (box-recovery.md § The plane-era recovery floor, <i>(c) The writes</i>) —
    /// and answer the warning it owes the admin, or <c>null</c> when nothing is
    /// owed. The leg is self-healing rather than event-driven: a live plane entry
    /// answers with no round trip, otherwise it checks roster membership, fetches
    /// the seed and merges it, so a claim, a co-admin granted later and a
    /// late-added device all converge the same way on their next connect. A call
    /// that could not even reach the leg (no connection) answers the shared
    /// "not attempted" warning, never a silent drop and never a key chosen here.
    /// Never throws.
    /// <para>No <c>ConfigureAwait(false)</c> — invoked from the UI-thread launch
    /// glue, continuations stay on the captured context per the WinUI
    /// off-thread-COMException rule.</para>
    /// </summary>
    internal static async Task<uniffi.fauna_core.LocalizedText?> SelfHealAsync(INestRpcClient rpc)
    {
        try
        {
            var warning = await rpc.SelfHealDeploymentSeedCustodyAsync();
            if (warning is not null)
            {
                ShellLog.Warn("DeploymentSeedCustody", $"custody leg: custody unconfirmed ({warning.@key})");
            }
            return warning;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("DeploymentSeedCustody",
                $"custody leg not attempted, recovery not confirmed protected: {ex.GetType().Name}: {ex.Message}");
            return FaunaFfiMethods.DeploymentSeedCustodyUnattemptedWarning();
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
