using System;
using System.IO;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Account-scoped client-state placement + erasure
/// (<c>docs/goal/architecture/apps/account-scoping.md</c> § The scoping
/// taxonomy class 1 + § Serialized switching). The windows twin of apple's
/// <c>AccountStateDir.swift</c>: <see cref="AccountStateDir"/> derives
/// <c>&lt;base&gt;/&lt;actor-id-hex&gt;/</c> over the <c>fauna_sync_engine::db</c> FFI
/// (real native code — no mocks; the same cross-language conformance basis as
/// <c>CryptoServiceTests</c>). No pre-scoping flat store is ever adopted — the
/// compat-remnant sweep removed that path (<c>version-compatibility.md</c>
/// § Dimension 2, the fourth ratified exception), pinned below as a refusal.
///
/// Each test points <see cref="AccountStateDir"/> at a fresh temp base via the
/// <c>FAUNA_E2E_DATA_DIR</c> redirect <see cref="BackupPaths.DataDir"/> already
/// honours, so no test touches the real profile.
/// </summary>
[Collection("AccountStateDirGlobal")]
public class AccountStateDirTests : IDisposable
{
    // Fresh, well-formed 64-char lowercase-hex actor ids per test.
    private readonly string ActorA = RandomActor();
    private readonly string ActorB = RandomActor();

    private static string RandomActor()
        => Convert.ToHexString(System.Security.Cryptography.RandomNumberGenerator.GetBytes(32))
            .ToLowerInvariant();

    private readonly string _base;
    private readonly string? _priorEnv;

    public AccountStateDirTests()
    {
        _priorEnv = Environment.GetEnvironmentVariable("FAUNA_E2E_DATA_DIR");
        _base = Path.Combine(Path.GetTempPath(), "fauna-acctstate-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_base);
        Environment.SetEnvironmentVariable("FAUNA_E2E_DATA_DIR", _base);
    }

    public void Dispose()
    {
        Environment.SetEnvironmentVariable("FAUNA_E2E_DATA_DIR", _priorEnv);
        try { Directory.Delete(_base, recursive: true); } catch { }
    }

    /// Seed a file inside an account's scope, resolving (and so creating) it.
    private string SeedScope(string actor)
    {
        var db = AccountStateDir.MlsDbPath(actor);
        File.WriteAllBytes(db, new byte[] { 1, 2, 3 });
        return db;
    }

    [Fact]
    public void MlsDbPath_IsUnderThePerAccountScope()
        => Assert.Equal(Path.Combine(_base, ActorA, "mls.db"), AccountStateDir.MlsDbPath(ActorA));

    [Fact]
    public void BackupAuditStatePath_IsUnderThePerAccountScope()
        => Assert.Equal(Path.Combine(_base, ActorA, "backup-audit-state.json"),
            AccountStateDir.BackupAuditStatePath(ActorA));

    /// The backup audit's replica anchor is the sync AGENT's actor scope, derived
    /// in shared Rust — under a test build, the harness's agent data dir
    /// (<c>FAUNA_E2E_SYNC_AGENT_DATA_DIR</c>), never the app's own base.
    [Fact]
    public void SyncAgentStateDir_IsTheAgentsActorScope_AndNullWithoutAnActor()
    {
        var prior = Environment.GetEnvironmentVariable("FAUNA_E2E_SYNC_AGENT_DATA_DIR");
        var agentBase = Path.Combine(_base, "agent");
        try
        {
            Environment.SetEnvironmentVariable("FAUNA_E2E_SYNC_AGENT_DATA_DIR", agentBase);
            Assert.Equal(Path.Combine(agentBase, ActorA), AccountStateDir.SyncAgentStateDir(ActorA));
            Assert.False(Directory.Exists(agentBase), "the agent owns its dir; resolving creates nothing");
            Assert.Null(AccountStateDir.SyncAgentStateDir(null));
            Assert.Null(AccountStateDir.SyncAgentStateDir(""));
        }
        finally
        {
            Environment.SetEnvironmentVariable("FAUNA_E2E_SYNC_AGENT_DATA_DIR", prior);
        }
    }

    [Fact]
    public void MalformedActorId_ResolvesUnderTheUnresolvedComponent_NeverTheBase()
    {
        var unresolved = Path.Combine(_base, "-unresolved-", "mls.db");
        Assert.Equal(unresolved, AccountStateDir.MlsDbPath("not-a-hex-id"));
        Assert.Equal(unresolved, AccountStateDir.MlsDbPath(null));
        Assert.Equal(unresolved, AccountStateDir.PureMlsDbPath(null));
        Assert.Equal(Path.Combine(_base, "-unresolved-", "backup-audit-state.json"),
            AccountStateDir.BackupAuditStatePath(null));
    }

    [Fact]
    public void PureMlsDbPath_CreatesNothing()
    {
        var path = AccountStateDir.PureMlsDbPath(ActorA);
        Assert.Equal(Path.Combine(_base, ActorA, "mls.db"), path);
        Assert.False(Directory.Exists(Path.Combine(_base, ActorA)),
            "the retired identity's resolver must never create a scope by asking");
    }

    [Fact]
    public void AFlatStoreAtTheBase_IsNeverAdopted()
    {
        // The refusal pin for the removed first-adopter hand-off: a store resting
        // directly in the base is not account state, and resolving an account's
        // scope neither copies nor claims it.
        File.WriteAllBytes(Path.Combine(_base, "mls.db"), new byte[] { 9, 9 });

        var scoped = AccountStateDir.MlsDbPath(ActorA);

        Assert.False(File.Exists(scoped), "a flat mls.db must never be adopted into a scope");
        Assert.False(File.Exists(Path.Combine(_base, "state-owner")),
            "no first-adopter marker is ever written");
    }

    [Fact]
    public void Erase_DropsTheAccountsScope_AndNoOther()
    {
        SeedScope(ActorA);
        var dbB = SeedScope(ActorB);

        AccountStateDir.Erase(ActorA);

        Assert.False(Directory.Exists(Path.Combine(_base, ActorA)),
            "the removed account's scoped stores must be gone");
        Assert.True(File.Exists(dbB), "another account's scope must survive a single-account erase");
    }

    [Fact]
    public void EraseAll_DropsEveryScope()
    {
        SeedScope(ActorA);
        SeedScope(ActorB);

        AccountStateDir.EraseAll();

        Assert.False(Directory.Exists(Path.Combine(_base, ActorA)));
        Assert.False(Directory.Exists(Path.Combine(_base, ActorB)));
    }

    [Fact]
    public void EraseAll_PreservesInstallScopedSiblings()
    {
        // Install-scoped state (logs, host-keyed pin store, config-replica, app
        // settings) is NOT named, so a sign-out leaves it intact.
        Directory.CreateDirectory(Path.Combine(_base, "logs"));
        File.WriteAllText(Path.Combine(_base, "logs", "fauna.log"), "boot");
        File.WriteAllText(Path.Combine(_base, "app-settings.json"), "{}");
        SeedScope(ActorA);

        AccountStateDir.EraseAll();

        Assert.True(File.Exists(Path.Combine(_base, "logs", "fauna.log")),
            "install-scoped state must survive a sign-out");
        Assert.True(File.Exists(Path.Combine(_base, "app-settings.json")));
    }
}

/// <summary>
/// Serialize <see cref="AccountStateDirTests"/> in isolation: it mutates the
/// process-global <c>FAUNA_E2E_DATA_DIR</c> env var that <c>BackupPaths.DataDir</c>
/// reads, so it must never run concurrently with any test that resolves that path
/// (the same discipline as <c>ActorScopedStaticsGlobal</c> over its process-global cache).
/// </summary>
[CollectionDefinition("AccountStateDirGlobal", DisableParallelization = true)]
public class AccountStateDirCollection { }
