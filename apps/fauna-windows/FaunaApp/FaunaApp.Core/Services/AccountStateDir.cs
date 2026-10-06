using System;
using System.IO;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Windows' <b>account-scoped</b> client-state home
/// (<c>docs/goal/architecture/apps/account-scoping.md</c> § The scoping
/// taxonomy, class 1 + § Serialized switching): <c>%LocalAppData%\Fauna\&lt;actor-id-hex&gt;\</c>
/// holds the stores whose meaning depends on who is signed in — the conversations
/// MLS store (<see cref="NestRpcClient"/>) and the client-side backup-audit state.
///
/// <para>The windows twin of apple's <c>FaunaKit/Core/AccountStateDir.swift</c>.
/// The derivation and the erasure are shared Rust (<c>fauna_sync_engine::db</c> via
/// <c>libs/fauna-ffi/src/account_state.rs</c>), the same code every other app
/// resolves through, so no process can invent a second layout. Nothing account-scoped
/// ever rests directly in <see cref="Base"/>: the pre-scoping flat layout and its
/// first-adopter adoption were removed by the compat-remnant sweep
/// (<c>version-compatibility.md</c> § Dimension 2, the fourth ratified exception —
/// no pre-scoping install exists).</para>
/// </summary>
public static class AccountStateDir
{
    private const string MlsDbName = "mls.db";
    // The class-4 client-side audit-loop state: scoped placement, re-derivable.
    private const string BackupAuditStateName = "backup-audit-state.json";

    /// <summary>
    /// <c>%LocalAppData%\Fauna</c> — the base the per-account subdirs live under, and
    /// the base the shared account-state and instance-lock FFI calls are keyed on.
    /// Redirectable under e2e via <c>FAUNA_E2E_DATA_DIR</c>
    /// (<see cref="BackupPaths.DataDir"/>).
    /// </summary>
    public static string Base => BackupPaths.DataDir;

    /// <summary>
    /// The conversations-rail MLS store, <c>&lt;base&gt;\&lt;actor&gt;\mls.db</c>, its
    /// directory created. A null/malformed actor id resolves under
    /// <see cref="UnresolvedActorComponent"/> — never onto <see cref="Base"/> itself.
    /// </summary>
    public static string MlsDbPath(string? actorIdHex)
    {
        var dir = ScopeDir(actorIdHex);
        try { Directory.CreateDirectory(dir); } catch { /* best-effort */ }
        return Path.Combine(dir, MlsDbName);
    }

    /// The scoped component a malformed/nil actor id resolves under — never a valid
    /// 64-hex actor id, so it can never collide with a real per-actor scoped dir, and
    /// never empty, so it can never collapse onto <see cref="Base"/> itself.
    private const string UnresolvedActorComponent = "-unresolved-";

    /// <summary>
    /// The account's MLS store path, read-only — no directory creation, unlike
    /// <see cref="MlsDbPath"/>. The windows twin of apple's
    /// <c>AccountStateDir.pureMlsDbPath</c>: this resolver's only caller is the
    /// RETIRED identity's own succession-retry path (<c>RetiredIdentityStorePath</c>),
    /// whose existence check must answer "no old state" for a store that was never
    /// written, never create one by asking.
    /// </summary>
    public static string PureMlsDbPath(string? actorIdHex)
        => Path.Combine(ScopeDir(actorIdHex), MlsDbName);

    /// <summary>
    /// The client-side backup-audit-loop state store's JSON file,
    /// <c>&lt;base&gt;\&lt;actor&gt;\backup-audit-state.json</c>
    /// (<c>docs/goal/ui/backups.md</c> § Audit-alert surface). A class-4 replica:
    /// the audit loop's last-passed/observed-high-water state is fully re-derivable
    /// (the next pass re-establishes it). Must stay actor-scoped: two accounts sharing one path would let
    /// one account's observation high-water suppress the other's freshness
    /// failures. A null/malformed actor id resolves as <see cref="MlsDbPath"/> does.
    /// </summary>
    public static string BackupAuditStatePath(string? actorIdHex)
    {
        var dir = ScopeDir(actorIdHex);
        try { Directory.CreateDirectory(dir); } catch { /* best-effort */ }
        return Path.Combine(dir, BackupAuditStateName);
    }

    /// <summary>
    /// The sync <b>agent's</b> per-actor state dir — where the agent's resident engines
    /// keep this account's bound folders' <c>fsid-&lt;ref&gt;.db</c> replicas
    /// (<c>%LocalAppData%\Fauna\sync\&lt;actor&gt;\</c>, or the harness's agent data dir
    /// in a test build). Windows runs no in-app engine for bound folders, so this is the
    /// replica the backup audit anchors the covered-folder mirror plane in
    /// (<c>backup-destinations.md</c> § Ordinary-folder coverage → <i>Retention + audit</i>).
    /// Derived in shared Rust (<c>backup_audit_local_agent_state_dir</c>, the same
    /// <c>local_agent_state_dir</c> linux and tui call), never re-derived here. <c>null</c>
    /// when the actor is unknown or the derivation fails — the declared absence, under
    /// which the audit keeps presence over the destination's list. Pure: nothing is
    /// created, the agent owns the dir.
    /// </summary>
    public static string? SyncAgentStateDir(string? actorIdHex)
    {
        if (string.IsNullOrEmpty(actorIdHex)) return null;
        try { return FaunaFfiMethods.BackupAuditLocalAgentStateDir(actorIdHex); }
        catch (Exception ex)
        {
            ShellLog.Warn("AccountStateDir", $"sync agent state dir failed: {ex.Message}");
            return null;
        }
    }

    /// <summary>Feed the client-side audit loop's observation high-water: this
    /// client has actually RENDERED activity stamped <paramref name="lastActivityMs"/>
    /// (epoch ms). Best-effort — a missed observation costs at most a weaker
    /// freshness comparison until the next render, and there is nothing a user
    /// could do about it (<c>docs/goal/ui/backups.md</c> § Audit-alert surface:
    /// "the load-bearing half, not an afterthought"). Call from the SAME render
    /// path that shows the user what it knows about the nest-originated kinds —
    /// the conversation list, on windows (mirrors linux/tui/web/android).</summary>
    public static void ObserveBackupAudit(string? actorIdHex, long lastActivityMs)
    {
        try { FaunaFfiMethods.BackupAuditObserve(BackupAuditStatePath(actorIdHex), lastActivityMs); }
        catch (Exception ex) { ShellLog.Warn("AccountStateDir", $"backup audit observe failed: {ex.Message}"); }
    }

    /// <summary>
    /// Erase ONE account's scoped stores — the "remove this account from this
    /// install" half of § Erasure follows scope.
    /// </summary>
    public static void Erase(string actorIdHex)
    {
        // `null` container dir = the shared per-user root (`StoreRoot::platform()`
        // in shared Rust), which is where this seat's W3 (account-data-plane.md § Workstreams) account store lives
        // (`NestRpcClient.StartAccountRuntimeAsync` passes the same `null`
        // storeContainer — a desktop `NotApplicable` cloud-backup posture, no
        // container to name, matching macOS). Android passes an explicit dir
        // instead only because its app-private files root is not the platform
        // default. Passing the root is load-bearing, not optional: an erase
        // that skips it strands the account store, which then refuses every
        // later sign-in ("belongs to a different writer") — see
        // `account_state_erase_all_scopes`' doc comment for the measured failure.
        try { FaunaFfiMethods.AccountStateEraseScope(Base, actorIdHex, null); }
        catch (Exception ex) { ShellLog.Warn("AccountStateDir", $"erase scope failed: {ex.Message}"); }
    }

    /// <summary>
    /// <b>May remove-account erase <paramref name="actorIdHex"/>?</b> The shared door
    /// (<c>fauna-ffi</c>'s <c>remove_account_blocked</c>) asked about exactly the two bases
    /// <see cref="Erase"/> sweeps — <see cref="Base"/> and the shared store root
    /// (<c>null</c> container, same reasoning as there). <c>null</c> = proceed; otherwise
    /// paint the case's <c>line</c> and remove nothing (<c>account-scoping.md</c> § Concurrent
    /// instances → <i>An erase refuses while a sibling serves the account</i>).
    ///
    /// <para><c>ownLock</c> and <c>servingHere</c> are <c>null</c> on purpose: windows serves
    /// through the process-global holder (<see cref="SessionInstance"/>), so the door puts
    /// this process's own locks down itself and reads the served account from that holder.
    /// Ask BEFORE the registry removal, which drops the account's secret slots.</para>
    ///
    /// <para>A door that throws degrades open (<c>null</c>), like every reader of these
    /// locks: the alternative is an account its owner can never remove.</para>
    /// </summary>
    internal static FfiEraseRemoveBlocked? RemoveAccountBlocked(string actorIdHex)
    {
        try { return FaunaFfiMethods.RemoveAccountBlocked(Base, actorIdHex, null, null, null); }
        catch (Exception ex)
        {
            ShellLog.Warn("AccountStateDir", $"remove-account probe failed, proceeding: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Erase EVERY account's scoped stores — the
    /// sign-out / factory-reset half of § Erasure follows scope, the store-side
    /// counterpart of the registry's whole-namespace <c>ClearAll()</c>. A "Sign Out"
    /// that left the MLS store on disk would be the same bug as one that left
    /// <c>fauna/{actor}/secret</c> behind: the user asked for their data off this
    /// device. Install-scoped siblings (logs, host-keyed pin store, config-replica,
    /// app settings) are unnamed and survive.
    /// </summary>
    /// <returns>
    /// What the sweep could NOT remove, folded across both roots. A non-empty
    /// <c>Survivors</c> means the signed-out user's readable data is still on
    /// this device, and the caller owes them a line — the count, never the
    /// paths (§ Erasure follows scope). <c>null</c> only when the call itself
    /// failed outright, which is not the same fact and must not be reported as
    /// a clean device.
    /// </returns>
    /// <remarks><c>internal</c>, not <c>public</c>, and that is forced rather than
    /// chosen: <c>uniffi-bindgen-cs</c> emits <b>every</b> generated type as
    /// <c>internal</c> — records, classes, interfaces and enums alike — so a
    /// <c>public</c> member of a <c>public</c> type that names one is CS0050,
    /// *"inconsistent accessibility: return type … is less accessible than method"*,
    /// and the whole FaunaApp.Core assembly fails to build. Both real callers are
    /// inside <c>[InternalsVisibleTo]</c> (<c>FaunaApp</c>, <c>FaunaApp.Tests</c>), so
    /// nothing is lost.
    ///
    /// <para>The rule is about EFFECTIVE accessibility, which is why the dozens of
    /// <c>public</c> members elsewhere in this assembly that take or return
    /// <c>Ffi*</c> types are fine: their containing type is <c>internal</c>
    /// (<c>FeedManagerHost</c>, <c>NestRpcClient</c>, the view-models), so the member
    /// is internal too and nothing is inconsistent. <c>AccountStateDir</c> is one of
    /// the few <c>public</c> classes here, which is the whole reason this member —
    /// and only this member — has to say so out loud. XAML is the separate case where
    /// mapping to an owned type is mandatory rather than optional: a classic
    /// <c>{Binding}</c> to an internal-typed property resolves to null with no error
    /// at all.</para></remarks>
    internal static FfiEraseSweep? EraseAll()
    {
        // `null` container dir — same reasoning as Erase() above. This is the
        // sign-out/factory-reset path, i.e. exactly the one whose omission the
        // shared doc calls "worse than stale bytes".
        //
        // ⚠ This no longer THROWS on a survivor — since 2026-09-09 it reports
        // one, because handling the outcome in a `catch` is precisely how every
        // seat came to paint a clean "Signed out" over a device that still held
        // the user's data. Windows is the platform where that is not
        // hypothetical: an open handle makes a file undeletable, measured three
        // times in one week. The `catch` below is for a HARD failure only.
        //
        // The paint is `App.SignOutHandler` → `ClearCredentialNamespace`, which
        // captures this return and hands it, with the credential read-back, to
        // `SignOutResidueSurface.Record` — recorded under `Base` so it outlives
        // the process, and painted as `identity_choice`'s `sign-out-residue`
        // view (`OnboardingViewModel.SignOutResidue`).
        //
        // ⚠ Member names below are the GENERATED ones, and they are neither
        // PascalCase nor collections: `uniffi-bindgen-cs` emits a positional record
        // whose parameters keep the Rust field names verbatim (`@survivors`,
        // `@residue`), and a Rust `Vec<String>` arrives as `string[]`. So it is
        // `sweep.survivors.Length`, never `sweep.Survivors.Count` — which is what
        // this method said until 2026-09-10, alongside a `public` signature naming
        // an `internal` type. Three compile errors in one method, none of them ever
        // built: `windows-app-build` had not run since the commit that added them.
        try
        {
            var sweep = FaunaFfiMethods.AccountStateEraseAllScopes(Base, null);
            if (sweep.survivors.Length > 0)
            {
                ShellLog.Warn("AccountStateDir",
                    $"{sweep.survivors.Length} path(s) SURVIVED the erase and still hold this user's data: "
                    + string.Join(", ", sweep.survivors));
            }
            return sweep;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("AccountStateDir", $"erase all scopes failed outright: {ex.Message}");
            return null;
        }
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    /// <summary>
    /// The account's scoped dir, <c>&lt;base&gt;\&lt;actor&gt;</c>, pure (no directory
    /// creation). Shared Rust refuses anything that is not a 64-char actor id rather
    /// than producing a stray directory; such a hex (or a null one) resolves under
    /// <see cref="UnresolvedActorComponent"/>.
    /// </summary>
    private static string ScopeDir(string? actorIdHex)
    {
        if (!string.IsNullOrEmpty(actorIdHex))
        {
            try { return FaunaFfiMethods.AccountStateDir(Base, actorIdHex); }
            catch { /* malformed hex falls through to the unresolved component */ }
        }
        return Path.Combine(Base, UnresolvedActorComponent);
    }
}
