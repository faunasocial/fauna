using System;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Services;

/// <summary>
/// The process-wide credential store, and the registry view onto it. Windows' twin
/// of linux's <c>client::secret_store()</c> free function — same shape, same
/// env-var-selected backend, so the two apps stay comparable.
///
/// <para>Everything that touches identity persistence goes through here, so the
/// app has exactly ONE store:</para>
/// <list type="bullet">
///   <item><description><see cref="Logical"/> — the logical-keyed
///     <c>FfiSecretStore</c> the shared multi-account registry sits on.</description></item>
///   <item><description><see cref="Registry"/> — a registry view (account list,
///     switch, the <c>LaunchPersistence</c> the launch machine routes on).</description></item>
/// </list>
/// <para>The app's pages read the served account's material through
/// <see cref="RegistrySessionAccount"/>, a read-only view over that same
/// registry.</para>
///
/// <para>There is no single-slot face beside the registry any more: the
/// pre-registry <c>legacy/*</c> rows are retired (<c>long-term-store.md</c>
/// § Downgrade mirror + abandoned-append recovery), and windows neither reads
/// nor writes them.</para>
///
/// <para><b>Backend selection is the shared crate's</b>, not this class's:
/// <see cref="SharedSecretBackend"/> hands every read and write to
/// <c>fauna_credential_store::CredentialStore</c>, which resolves Credential
/// Manager, or the harness's file when <c>FAUNA_E2E_CREDENTIAL_DIR</c> is set in a
/// test-capable build — the same <c>cred_file_dir()</c> linux, tui and the sync
/// agent resolve through. That is what lets a test pre-seed a whole
/// multi-account registry before launch with no app code, and keeps the suite
/// out of the dev machine's real Credential Manager.</para>
/// </summary>
internal static class CredentialStore
{
    /// <summary>Default file-backend namespace when the harness sets no <c>FAUNA_KEYRING_APP</c>.</summary>
    private const string DefaultNamespace = "fauna-windows";

    private static readonly object Gate = new();
    private static LogicalSecretStore? _logical;

    /// <summary>
    /// The one logical-keyed store. Built once per process; the backend choice
    /// cannot change under a running app.
    ///
    /// <para>Also the <c>installStore</c> every windows call to
    /// <c>FfiAccountRegistry.DeviceIdForActor</c> passes
    /// (<c>sync-agent-credentials.md</c> § Credential model, the RULED
    /// 2026-09-20 block) — safe because windows has exactly ONE credential
    /// store: sign-out (<c>App.xaml.cs</c>'s <c>ClearCredentialNamespace</c> →
    /// <c>Registry().ClearAll()</c>, the only bulk eraser) never names
    /// <c>install/device_secret</c>. An app needing a SECOND store for this (android's wholesale
    /// <c>SecureStorage.clear()</c>) would pass a store its own reset does not
    /// reach; windows needs no such split.</para>
    /// </summary>
    internal static LogicalSecretStore Logical
    {
        get
        {
            lock (Gate)
            {
                return _logical ??= Build();
            }
        }
    }

    private static LogicalSecretStore Build()
    {
        // Via E2eEnv for the same reason as the dir below (convention 15) —
        // this read is not `FAUNA_E2E_`-prefixed, so the 2026-08-11 sweep's
        // prefix-matching pin never met it.
        var harnessNamespace = E2eEnv.KeyringApp;
        var ns = string.IsNullOrEmpty(harnessNamespace) ? DefaultNamespace : harnessNamespace;

        // The harness's credential-dir redirect is read by the shared crate, gated
        // there (`cred_file_dir`, compiled out of a release build — convention 15):
        // ungated, it would let whoever controlled the launch environment relocate
        // the whole credential store, where the identity secret is read AND
        // written, out of Credential Manager and into a directory of their choosing.
        return new LogicalSecretStore(new SharedSecretBackend(ns));
    }

    /// <summary>
    /// A registry view over the one store, its mutations serialized under the
    /// install-scoped cross-process lock (<c>long-term-store.md</c> § Multi-account
    /// evolution → <i>Cross-process mutation lock</i>): two instances on one OS
    /// login each rewrite the one <c>fauna/index</c> blob, and without the lock one
    /// rewrite can swallow the other's. The lock lives in
    /// <see cref="AccountStateDir.Base"/>, the install-scoped base the instance lock
    /// already uses. Cheap (a stateless view), and <c>IDisposable</c> —
    /// <c>using</c> it at each call site, exactly as linux constructs its registry
    /// per use.
    ///
    /// <para><b>The app's ONLY registry builder.</b> A registry built anywhere else
    /// carries no lock, and nothing observable fails — the write lands while
    /// skipping the lock every other writer takes. <c>RegistryConstructionCensusTests</c>
    /// fails the build if a second one appears.</para>
    /// </summary>
    internal static FfiAccountRegistry Registry() =>
        FfiAccountRegistry.NewWithLockDir(Logical, AccountStateDir.Base);

}
