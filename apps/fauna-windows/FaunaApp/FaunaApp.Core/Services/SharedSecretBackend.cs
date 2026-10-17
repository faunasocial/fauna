using System;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The raw key/value backend for the logical-keyed secret store: the shared
/// Rust credential store (<c>fauna_credential_store::CredentialStore</c>),
/// reached over the FFI as <c>credential_store_get</c>/<c>set</c>/<c>delete</c>.
///
/// <para><b>One backend, and the crate picks the arm</b> — exactly as it does for
/// the terminal app and the sync agent:</para>
/// <list type="bullet">
///   <item><description><b>Credential Manager</b> (production) — one generic
///     credential per row, <c>TargetName</c> = <c>"{namespace}/{key}"</c>, a UTF-8
///     blob, and <c>CRED_PERSIST_LOCAL_MACHINE</c>: per-user, and <b>never
///     roamed</b> to the person's other PCs, which is what <c>apps/common.md</c>
///     § Credential storage → <i>What a device's own backup carries</i> asks of a
///     store. The terminal app and the sync agent write the same arm, so all
///     three read each other's grammar by construction.</description></item>
///   <item><description><b>The harness file</b> — a flat JSON map at
///     <c>{FAUNA_E2E_CREDENTIAL_DIR}/{namespace}.json</c>, selected by the crate's
///     own <c>cred_file_dir()</c>, which is compiled out of a release build
///     (convention 15). This is what lets an e2e seed a whole multi-account
///     registry before launch with no app code, and keeps the suite out of the
///     dev machine's real Credential Manager. Its writes are serialized across
///     processes and replace the file atomically — the property two instances
///     sharing one credential dir need, and the one the shell's own copy of
///     this arm lacked: it rewrote the file in place, unlocked, and lost
///     registry writes to a sibling instance's read
///     (<c>credential file write failed: … being used by another process</c>,
///     measured 2026-10-10).</description></item>
/// </list>
///
/// <para>Keyed by Resource alone (the UserName is ignored): every logical key
/// resolves to a distinct Resource, and the namespace is this backend's own.</para>
///
/// <para><b>Failures are swallowed by contract</b>, as on every platform store:
/// the shared arm's write is best-effort and log-only, and a throw here would
/// take down the launch path. One Credential Manager value is capped at 2560
/// bytes (<c>fauna_credential_store::MAX_ITEM_VALUE_BYTES</c>, its own limit), so
/// a caller that needs a write to have landed reads it back.</para>
/// </summary>
internal sealed class SharedSecretBackend : ISecretBackend
{
    private readonly string _namespace;

    internal SharedSecretBackend(string ns)
    {
        _namespace = ns;
    }

    public string? Get(string resource, string user)
    {
        try
        {
            return FaunaFfiMethods.CredentialStoreGet(_namespace, resource);
        }
        catch (Exception ex)
        {
            ShellLog.Error("SecretStore", $"[SharedSecretBackend] get {resource} failed: {ex.Message}");
            return null;
        }
    }

    public void Set(string resource, string user, string value)
    {
        try
        {
            FaunaFfiMethods.CredentialStoreSet(_namespace, resource, value);
        }
        catch (Exception ex)
        {
            ShellLog.Error("SecretStore", $"[SharedSecretBackend] set {resource} failed: {ex.Message}");
        }
    }

    public void Delete(string resource, string user)
    {
        try
        {
            FaunaFfiMethods.CredentialStoreDelete(_namespace, resource);
        }
        catch (Exception ex)
        {
            ShellLog.Error("SecretStore", $"[SharedSecretBackend] delete {resource} failed: {ex.Message}");
        }
    }
}
