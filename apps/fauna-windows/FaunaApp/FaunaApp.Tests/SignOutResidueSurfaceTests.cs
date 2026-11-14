using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="SignOutResidueSurface"/> — the windows seat of <b>the sign-out record</b>
/// and the residue surface (<c>docs/goal/architecture/apps/account-scoping.md</c>
/// § Erasure follows scope → <i>The sign-out record is on all 7 apps</i>). The record,
/// the routine and every word of the line are shared Rust behind the
/// <c>fauna-ffi</c> face (real native FFI — nothing here is mocked but the credential
/// backend and the localizer), so this pins only the windows-side plumbing: the
/// gesture's record names every listed account and the served one, a launch over a
/// record a dead sign-out left wipes, erases and clears it, a launch beside a window
/// that still serves a recorded account fails closed onto the refusal line, what the
/// gesture's own finish leaves is painted and Remove Again re-runs the routine, and
/// a signed-in launch leaves the residue record alone.
///
/// <para>Every test runs in a fresh temp install base with its own store container,
/// so nothing here reads or writes the real <c>%LocalAppData%\Fauna</c>, and the sync
/// agent's pipe is pointed at a name nothing serves, so a launch's dismissal of the
/// agent fails fast instead of reaching this machine's real agent.</para>
///
/// <para>Serializes with the other <c>Strings.Initialize</c>-mutating test classes
/// under xUnit's default parallel-by-class runner — see
/// <c>StringsGlobalCollection</c>.</para>
/// </summary>
[Collection("StringsGlobal")]
public class SignOutResidueSurfaceTests : IDisposable
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    /// <summary>An in-memory stand-in for Credential Manager.</summary>
    private sealed class MemoryBackend : ISecretBackend
    {
        private readonly Dictionary<string, string> _rows = new();
        public string? Get(string resource) =>
            _rows.TryGetValue(resource, out var v) ? v : null;
        public void Set(string resource, string value) => _rows[resource] = value;
        public void Delete(string resource) => _rows.Remove(resource);
        public IReadOnlyCollection<string> Keys => _rows.Keys;
    }

    private const string Served = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    private const string RecordFile = "sign-out-record.json";
    private const string ResidueFile = "sign-out-residue.json";
    private const string PipeEnv = "FAUNA_E2E_SYNC_PIPE";

    private readonly string _root;
    private readonly string? _priorPipe;
    private readonly MemoryBackend _backend = new();
    private readonly ResidueSeat _seat;

    public SignOutResidueSurfaceTests()
    {
        // The real templates so the {count} substitution is exercised, not
        // just the key fallback (i18n/strings/en.yaml).
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["settings/sign_out_residue"] =
                "Signed out, but {count} item(s) of your data could not be removed "
                + "from this device — another program may still be using them. "
                + "Press Remove Again to try once more.",
            ["settings/sign_out_residue_credentials"] =
                "Signed out, but your sign-in credentials could not be removed from "
                + "this device — its secure storage may be locked or unavailable. "
                + "Press Remove Again to try once more.",
            ["settings/sign_out_residue_retry_blocked_other_window"] =
                "Not removed — another Fauna window is using some of this data. "
                + "Close it, then press Remove Again.",
        }));

        _priorPipe = Environment.GetEnvironmentVariable(PipeEnv);
        Environment.SetEnvironmentVariable(PipeEnv, "fauna-row-4317-no-agent-" + Guid.NewGuid().ToString("N"));

        _root = Path.Combine(Path.GetTempPath(), "fauna-residue-" + Guid.NewGuid().ToString("N"));
        var installBase = Path.Combine(_root, "app");
        var storeContainer = Path.Combine(_root, "store");
        Directory.CreateDirectory(installBase);
        Directory.CreateDirectory(storeContainer);
        var store = new LogicalSecretStore(_backend);
        _seat = new ResidueSeat(installBase, storeContainer, () => new FfiAccountRegistry(store));
    }

    public void Dispose()
    {
        Environment.SetEnvironmentVariable(PipeEnv, _priorPipe);
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    /// <summary>A signed-in account; its actor id.</summary>
    private string SignIn(char secretFill)
    {
        using var registry = _seat.Registry();
        return registry.AddAccount(new string(secretFill, 64), "https://a.example", null);
    }

    private string[] Listed()
    {
        using var registry = _seat.Registry();
        return registry.List().Select(e => e.@actorId).ToArray();
    }

    /// <summary><paramref name="actor"/>'s scope under the install base, holding one file.</summary>
    private string ScopeOf(string actor)
    {
        var dir = Path.Combine(_seat.BaseDir, actor);
        Directory.CreateDirectory(dir);
        File.WriteAllText(Path.Combine(dir, "mls.db"), "the user's data");
        return dir;
    }

    /// <summary>
    /// Hold the scope's file open with no sharing at all — windows' own fault shape
    /// (an open handle makes a file undeletable): the erase cannot remove it until
    /// the handle is disposed.
    /// </summary>
    private static FileStream Hold(string scope) =>
        new(Path.Combine(scope, "mls.db"), FileMode.Open, FileAccess.Read, FileShare.None);

    private string RecordPath => Path.Combine(_seat.BaseDir, RecordFile);
    private string ResiduePath => Path.Combine(_seat.BaseDir, ResidueFile);

    [Fact]
    public void TheGesture_RecordsEveryListedAccountAndTheServedOne_OnDisk()
    {
        var listed = SignIn('1');

        var recorded = SignOutResidueSurface.RecordSignOut(_seat, Served);

        Assert.True(File.Exists(RecordPath), "the decision must be on disk before the gesture's first await");
        Assert.Equal(new[] { listed, Served }, recorded);
    }

    /// <summary>
    /// The app died after the gesture's record — inside the runtime's stop, before
    /// the wipe: the next launch wipes, erases the recorded account's scope and
    /// clears the record, and lands signed out with nothing to say.
    /// </summary>
    [Fact]
    public void ALaunch_OverARecordADeadSignOutLeft_WipesErasesAndClears()
    {
        var actor = SignIn('1');
        var scope = ScopeOf(actor);
        Assert.NotNull(SignOutResidueSurface.RecordSignOut(_seat, actor));

        var outcome = SignOutResidueSurface.Finish(_seat, FfiFinishCaller.Launch, null);

        Assert.False(outcome.FailsClosed);
        Assert.Null(outcome.Residue);
        Assert.Empty(Listed());
        Assert.DoesNotContain(_backend.Keys, k => k.Contains(actor, StringComparison.OrdinalIgnoreCase));
        Assert.False(Directory.Exists(scope), "the recorded scope is erased");
        Assert.False(File.Exists(RecordPath), "and the record cleared");

        Assert.Equal(SignOutFinishOutcome.Nothing, SignOutResidueSurface.Finish(_seat, FfiFinishCaller.Launch, null));
    }

    [Fact]
    public void ALaunch_WithNothingRecorded_PaintsNothing_AndKeepsTheAccount()
    {
        var actor = SignIn('1');

        Assert.Equal(SignOutFinishOutcome.Nothing, SignOutResidueSurface.Finish(_seat, FfiFinishCaller.Launch, null));
        Assert.Equal(new[] { actor }, Listed());
    }

    /// <summary>
    /// Another window still serves a recorded account: the launch's finish wipes and
    /// erases nothing and FAILS CLOSED onto the refusal line; once that window is
    /// gone, Remove Again runs the routine and finishes the sign-out.
    /// </summary>
    [Fact]
    public void ALaunch_BesideAWindowThatServesARecordedAccount_FailsClosed_UntilRemoveAgainFinishes()
    {
        var actor = SignIn('1');
        var scope = ScopeOf(actor);
        Assert.NotNull(SignOutResidueSurface.RecordSignOut(_seat, null));

        SignOutFinishOutcome outcome;
        var sibling = FaunaFfiMethods.AcquireAccountInstanceLockShared(_seat.BaseDir, actor, _seat.StoreContainerDir);
        Assert.NotNull(sibling);
        using (sibling)
        {
            outcome = SignOutResidueSurface.Finish(_seat, FfiFinishCaller.Launch, null);

            Assert.True(outcome.FailsClosed, "a refused finish whose wipe is still owed fails closed");
            Assert.NotNull(outcome.Residue);
            Assert.Contains("another Fauna window", outcome.Residue!.Line);
            Assert.Equal(new[] { actor }, Listed());
            Assert.True(File.Exists(Path.Combine(scope, "mls.db")), "a served store is never touched");
            Assert.True(File.Exists(RecordPath), "the record stands");

            // Remove Again while that window still serves: refused again, nothing touched.
            var still = outcome.Residue.Retry();
            Assert.NotNull(still);
            Assert.Contains("another Fauna window", still!.Line);
            Assert.Equal(new[] { actor }, Listed());
        }

        Assert.Null(outcome.Residue!.Retry());
        Assert.Empty(Listed());
        Assert.False(Directory.Exists(scope));
        Assert.False(File.Exists(RecordPath));
    }

    /// <summary>
    /// The gesture's own finish over its record, with a scope another program holds
    /// open: the residue line is painted (the count, never the path) and recorded;
    /// once the handle is gone, Remove Again runs the routine and closes the view.
    /// </summary>
    [Fact]
    public void TheGesture_PaintsWhatItsFinishLeft_AndRemoveAgainFinishesIt()
    {
        var actor = SignIn('1');
        var scope = ScopeOf(actor);
        var written = SignOutResidueSurface.RecordSignOut(_seat, actor);

        SignOutFinishOutcome outcome;
        using (Hold(scope))
        {
            outcome = SignOutResidueSurface.Finish(_seat, FfiFinishCaller.Gesture, written);

            Assert.False(outcome.FailsClosed);
            Assert.NotNull(outcome.Residue);
            Assert.Contains("1 item(s)", outcome.Residue!.Line);
            Assert.Contains("Remove Again", outcome.Residue.Line);
            Assert.DoesNotContain(_root, outcome.Residue.Line);
            Assert.Empty(Listed());
            Assert.True(File.Exists(ResiduePath), "what the erase left outlives the process");
            Assert.False(File.Exists(RecordPath), "the sign-out record hands over to the residue record");

            Assert.NotNull(outcome.Residue.Retry());
            Assert.True(File.Exists(Path.Combine(scope, "mls.db")));
        }

        Assert.Null(outcome.Residue.Retry());
        Assert.False(Directory.Exists(scope), "the recorded scope is gone");
        Assert.False(File.Exists(ResiduePath), "and so is the record");
    }

    /// <summary>
    /// A launch with an account in the registry is not the user the residue was
    /// reported to: the residue record waits, untouched (decision 3, the registry gate).
    /// </summary>
    [Fact]
    public void ASignedInLaunch_LeavesTheResidueRecordAlone()
    {
        var actor = SignIn('1');
        var scope = ScopeOf(actor);
        var written = SignOutResidueSurface.RecordSignOut(_seat, actor);
        using (Hold(scope))
        {
            Assert.NotNull(SignOutResidueSurface.Finish(_seat, FfiFinishCaller.Gesture, written).Residue);
        }
        var signedIn = SignIn('2');

        Assert.Equal(SignOutFinishOutcome.Nothing, SignOutResidueSurface.Finish(_seat, FfiFinishCaller.Launch, null));
        Assert.True(File.Exists(Path.Combine(scope, "mls.db")), "nothing was swept");
        Assert.True(File.Exists(ResiduePath), "the record waits");
        Assert.Equal(new[] { signedIn }, Listed());
    }
}
