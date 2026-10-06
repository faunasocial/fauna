namespace FaunaApp.Core.Services;

/// <summary>
/// The ONE place the windows app reads a <c>FAUNA_E2E_*</c> environment variable
/// — the C# arm of e2e-conventions.md convention 15 ("the automation surface is
/// compiled out of release artifacts").
///
/// <para><b>Why a type rather than a gate at each call site.</b> Convention 15's
/// C# mechanism is <c>#if DEBUG</c> (plus the opt-in <c>FAUNA_E2E_AGENT</c> flavor
/// for the one automatable Release build, <c>tests/platform/windows/test_installer.py</c>).
/// Sprinkling that directive over sixteen call sites across two assemblies is how
/// the gate decays: every new seam is a fresh chance to forget it, and a missed one
/// is invisible until someone greps a shipped MSI. Funnelling every read through
/// one gated file makes the boundary a single reviewable surface and lets a tier_1
/// structural pin enforce it by grep
/// (<c>test_ffi_flavor_split.py::test_no_windows_app_source_reads_a_fauna_e2e_var_directly</c>).</para>
///
/// <para><b>The shape is convention 15's own:</b> a gated real plus a
/// <i>same-signature</i> production twin (§ convention 15's shared-Rust bullet —
/// "a same-signature no-op twin wherever the caller is plumbing the app compiles
/// unconditionally"; tui's <c>e2e_gated</c> is the Rust reference). The twin returns
/// <c>null</c> for every variable, which is exactly what an unset variable already
/// meant — so <b>every call site's production arm is unchanged by construction</b>,
/// and the env-var NAMES themselves never appear in the Release IL.</para>
///
/// <para><b>Raw values, not predicates, on purpose.</b> Call sites historically
/// disagree on how to test <c>FAUNA_E2E_BRIDGE</c>: some use <c>is not null</c>
/// (an empty value counts as e2e), others <c>string.IsNullOrEmpty</c> (it does not).
/// The harness always sets a real bridge URL so the two agree in practice, but
/// collapsing them here would be a silent behaviour change smuggled in under a
/// security fix. Each caller keeps its own predicate; unifying them is a separate,
/// visible decision.</para>
///
/// <para>The runtime checks at those call sites <b>stay</b> — convention 15 is
/// explicit that they are the inner convenience switch (agent-on vs agent-off
/// <i>within</i> a test-capable build), never the boundary. This file is the
/// boundary.</para>
/// </summary>
internal static class E2eEnv
{
#if DEBUG || FAUNA_E2E_AGENT

    /// <summary>The e2e bridge URL the test agent polls; unset outside a harness launch.</summary>
    internal static string? Bridge => Environment.GetEnvironmentVariable("FAUNA_E2E_BRIDGE");

    /// <summary>
    /// The bridge a <i>spawned child</i> should report to. Never inherited from this
    /// process — see <c>InstanceSpawner</c> for why a shared bridge makes the child
    /// steal the parent's commands.
    /// </summary>
    internal static string? ChildBridge => Environment.GetEnvironmentVariable("FAUNA_E2E_CHILD_BRIDGE");

    /// <summary>
    /// Selects the file-backed credential store over Credential Manager, and the
    /// file-backed re-auth verdict over Windows Hello. The highest-severity read in
    /// this file on both counts: whoever sets it names where the identity secret is
    /// read and written, and supplies the re-auth answer.
    /// </summary>
    internal static string? CredentialDir => Environment.GetEnvironmentVariable("FAUNA_E2E_CREDENTIAL_DIR");

    /// <summary>
    /// Overrides the credential-store <i>namespace</i> so parallel e2e runs never
    /// sweep each other's credentials. Lives here despite lacking the
    /// <c>FAUNA_E2E_</c> prefix — which is exactly why the 2026-08-11 sweep never
    /// met it, since the pin it added matches on the prefix. Lesser class than
    /// <see cref="CredentialDir"/> (a namespace within the user's own vault, not a
    /// relocation out of it), but still a harness knob no deployment sets, so
    /// convention 15 compiles it out. Mirrors the shared Rust
    /// <c>fauna_credential_store::keyring_app_override()</c>.
    /// </summary>
    internal static string? KeyringApp => Environment.GetEnvironmentVariable("FAUNA_KEYRING_APP");

    /// <summary>Redirects the canonical Fauna data dir (MLS store, nest-identity pins, backup state).</summary>
    internal static string? DataDir => Environment.GetEnvironmentVariable("FAUNA_E2E_DATA_DIR");

    /// <summary>Where the e2e snapshot-file saver writes, in place of the native save picker.</summary>
    internal static string? DownloadDir => Environment.GetEnvironmentVariable("FAUNA_E2E_DOWNLOAD_DIR");

    /// <summary>Path of the shared agent/shell trace file (<see cref="Logs.E2eTrace"/>).</summary>
    internal static string? AgentLog => Environment.GetEnvironmentVariable("FAUNA_E2E_AGENT_LOG");

    /// <summary>Opt-in: let the hydration session drive the REAL per-user sync agent under a bridge.</summary>
    internal static string? RealSyncAgent => Environment.GetEnvironmentVariable("FAUNA_E2E_REAL_SYNC_AGENT");

    /// <summary>
    /// The pipe leaf this run's own agent rendezvouses on, in place of the
    /// machine-global <c>\\.\pipe\fauna-sync.&lt;SID&gt;</c>. Read here only to
    /// answer <i>"has the harness told us which agent to drive?"</i>; the value
    /// itself is consumed by shared Rust (<c>fauna_ipc::endpoint</c>'s
    /// <c>E2E_PIPE_ENV</c>), which builds the full path and passes it to the
    /// agent as <c>--pipe-name</c>.
    /// </summary>
    internal static string? SyncPipe => Environment.GetEnvironmentVariable("FAUNA_E2E_SYNC_PIPE");

    /// <summary>
    /// The one <c>fauna-sync-agent.exe</c> this run may spawn. Paired with
    /// <see cref="SyncPipe"/> it is what makes "never the box's installed agent"
    /// STRUCTURAL rather than conventional: shared Rust's spawner
    /// (<c>agent_spawner::WindowsDetachedSpawner</c>) spawns NOTHING when a pin
    /// misses, where an unpinned spawn walks its candidate list down to
    /// <c>%ProgramFiles%</c>. Read here only as a presence check.
    /// </summary>
    internal static string? SyncAgentBin => Environment.GetEnvironmentVariable("FAUNA_E2E_SYNC_AGENT_BIN");

#else

    // Production twins. Same signatures, all null — indistinguishable to every
    // caller from "the variable is not set", which is the only state a shipped app
    // was ever supposed to observe.
    internal static string? Bridge => null;
    internal static string? ChildBridge => null;
    internal static string? CredentialDir => null;
    internal static string? KeyringApp => null;
    internal static string? DataDir => null;
    internal static string? DownloadDir => null;
    internal static string? AgentLog => null;
    internal static string? RealSyncAgent => null;
    internal static string? SyncPipe => null;
    internal static string? SyncAgentBin => null;

#endif
}
