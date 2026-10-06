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
/// <para><b>Backend selection.</b> <c>FAUNA_E2E_CREDENTIAL_DIR</c> selects the
/// file backend (E2E); otherwise Credential Manager, through the shared Rust arm
/// (<see cref="CredManSecretBackend"/>). This is what lets a test
/// pre-seed a whole multi-account registry before launch with no app code — and
/// keeps the suite out of the dev machine's real Credential Manager. Mirrors
/// linux's <c>cred_file_dir()</c>.</para>
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

        // Via E2eEnv so the read is compiled out of release builds (convention 15).
        // Ungated, this let whoever controlled the launch environment relocate the
        // whole credential store — where the identity secret is read AND written —
        // out of Credential Manager and into a directory of their choosing.
        var dir = E2eEnv.CredentialDir;
        if (!string.IsNullOrEmpty(dir))
        {
            return new LogicalSecretStore(new FileSecretBackend(dir, ns));
        }

        return new LogicalSecretStore(new CredManSecretBackend(ns));
    }

    /// <summary>
    /// A registry view over the one store. Cheap (a stateless view), and
    /// <c>IDisposable</c> — <c>using</c> it at each call site, exactly as linux
    /// constructs <c>AccountRegistry::new(store)</c> per use.
    /// </summary>
    internal static FfiAccountRegistry Registry() => new FfiAccountRegistry(Logical);

}
