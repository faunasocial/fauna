using System;
using System.Collections.Generic;
using System.IO;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="SignOutResidueSurface"/> — the windows seat of the sign-out
/// residue surface (<c>docs/goal/architecture/apps/account-scoping.md</c>
/// § Erasure follows scope → <i>the residue surface</i>). The record, the
/// re-sweep and every word of the line are shared Rust behind the
/// <c>fauna-ffi</c> residue face (real native FFI — nothing here is mocked but
/// the credential backend and the localizer), so this pins only the
/// windows-side plumbing: a clean erase paints nothing and keeps no record, a
/// surviving scope paints the Remove Again copy and is recorded under the
/// install base, Remove Again and the signed-out launch both run the shared
/// re-sweep, and a filesystem erase that failed outright still answers for the
/// credential half.
///
/// <para>Every test runs in a fresh temp install base with its own store
/// container, so nothing here reads or writes the real
/// <c>%LocalAppData%\Fauna</c>.</para>
///
/// <para>Serializes with the other <c>Strings.Initialize</c>-mutating test
/// classes under xUnit's default parallel-by-class runner — see
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
        public string? Get(string resource, string user) =>
            _rows.TryGetValue(resource, out var v) ? v : null;
        public void Set(string resource, string user, string value) => _rows[resource] = value;
        public void Delete(string resource, string user) => _rows.Remove(resource);
    }

    private const string Actor = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
    private const string RecordFile = "sign-out-residue.json";

    private readonly string _root;
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
        }));

        _root = Path.Combine(Path.GetTempPath(), "fauna-residue-" + Guid.NewGuid().ToString("N"));
        var installBase = Path.Combine(_root, "app");
        var storeContainer = Path.Combine(_root, "store");
        Directory.CreateDirectory(installBase);
        Directory.CreateDirectory(storeContainer);
        var store = new LogicalSecretStore(new MemoryBackend());
        _seat = new ResidueSeat(installBase, storeContainer, () => new FfiAccountRegistry(store));
    }

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    /// <summary>An actor scope under the install base holding one file.</summary>
    private string Scope()
    {
        var dir = Path.Combine(_seat.BaseDir, Actor);
        Directory.CreateDirectory(dir);
        File.WriteAllText(Path.Combine(dir, "mls.db"), "the user's data");
        return dir;
    }

    private static FfiEraseSweep Sweep(params string[] survivors) => new(
        erased: 0,
        survivors: survivors,
        residue: new FfiEraseResidueView(
            survivors: (uint)survivors.Length,
            credentialsSurvived: false,
            owesWork: survivors.Length > 0));

    private static readonly FfiCredentialSweep CleanCredentials =
        new(survivors: Array.Empty<string>(), wipeFailed: false);

    private static readonly FfiCredentialSweep SurvivingCredentials =
        new(survivors: new[] { "fauna/aa11/secret" }, wipeFailed: false);

    private string RecordPath => Path.Combine(_seat.BaseDir, RecordFile);

    [Fact]
    public void Record_PaintsNothing_AndKeepsNoRecord_ForACleanErase()
    {
        Assert.Null(SignOutResidueSurface.Record(_seat, Sweep(), CleanCredentials));
        Assert.False(File.Exists(RecordPath));
    }

    [Fact]
    public void Record_PaintsTheRemoveAgainLine_AndRecordsIt_WhenAScopeSurvives()
    {
        var scope = Scope();

        var surface = SignOutResidueSurface.Record(_seat, Sweep(scope), CleanCredentials);

        Assert.NotNull(surface);
        Assert.Contains("1 item(s)", surface!.Line);
        // The Rendered copy: it names the control the view paints beside it.
        Assert.Contains("Remove Again", surface.Line);
        // Never the path — the count goes to the user, the paths to the log.
        Assert.DoesNotContain(_root, surface.Line);
        Assert.True(File.Exists(RecordPath), "the record must outlive the process");
    }

    [Fact]
    public void RemoveAgain_FinishesTheErase_AndClosesTheView()
    {
        var scope = Scope();
        var surface = SignOutResidueSurface.Record(_seat, Sweep(scope), CleanCredentials);
        Assert.NotNull(surface);

        var left = surface!.Retry();

        Assert.Null(left);
        Assert.False(Directory.Exists(scope), "the recorded scope is gone");
        Assert.False(File.Exists(RecordPath), "and so is the record");
    }

    /// <summary>
    /// A filesystem erase that failed outright hands back no sweep at all — and
    /// the credential half still owes its line, without the key name.
    /// </summary>
    [Fact]
    public void Record_StillNamesTheCredentials_WhenTheFilesystemEraseFailedOutright()
    {
        var surface = SignOutResidueSurface.Record(_seat, null, SurvivingCredentials);

        Assert.NotNull(surface);
        Assert.Contains("sign-in credentials", surface!.Line);
        Assert.DoesNotContain("fauna/aa11", surface.Line);

        Assert.Null(SignOutResidueSurface.Record(_seat, null, CleanCredentials));
    }

    [Fact]
    public void ASignedOutLaunch_ResweepsSilently_AndPaintsOnlyWhatIsLeft()
    {
        Assert.Null(SignOutResidueSurface.RecheckAtLaunch(_seat));

        var scope = Scope();
        Assert.NotNull(SignOutResidueSurface.Record(_seat, Sweep(scope), CleanCredentials));

        // The scope can be removed now, so the launch finishes it without a word.
        Assert.Null(SignOutResidueSurface.RecheckAtLaunch(_seat));
        Assert.False(Directory.Exists(scope));
        Assert.False(File.Exists(RecordPath));
    }

    /// <summary>
    /// A launch with an account in the registry is not the user the residue was
    /// reported to: shared Rust leaves the record untouched.
    /// </summary>
    [Fact]
    public void ASignedInLaunch_LeavesTheRecordAlone()
    {
        var scope = Scope();
        Assert.NotNull(SignOutResidueSurface.Record(_seat, Sweep(scope), CleanCredentials));
        using (var registry = _seat.Registry())
        {
            registry.AddAccount(new string('1', 64), "https://a.example", null);
        }

        Assert.Null(SignOutResidueSurface.RecheckAtLaunch(_seat));
        Assert.True(File.Exists(Path.Combine(scope, "mls.db")), "nothing was swept");
        Assert.True(File.Exists(RecordPath), "the record waits");
    }
}
