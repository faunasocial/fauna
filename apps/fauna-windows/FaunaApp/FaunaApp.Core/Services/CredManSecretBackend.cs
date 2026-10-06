using System;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Windows Credential Manager as the raw key/value backend for the
/// logical-keyed secret store — the production arm of <see cref="ISecretBackend"/>.
///
/// <para><b>It is the shared Rust arm, reached over the FFI</b>
/// (<c>fauna_credential_store::win_credman</c>, exported as
/// <c>native_keyring_get</c>/<c>set</c>/<c>delete</c>), not a second
/// implementation: one generic credential per row, <c>TargetName</c> =
/// <c>"{namespace}/{key}"</c>, a UTF-8 blob, and
/// <c>CRED_PERSIST_LOCAL_MACHINE</c> — per-user, and <b>never roamed</b> to the
/// person's other PCs, which is what <c>apps/common.md</c> § Credential storage
/// → <i>What a device's own backup carries</i> asks of a store. The terminal
/// app and the sync agent write the same arm, so all three read each other's
/// grammar by construction.</para>
///
/// <para>Keyed by Resource alone (the UserName is ignored), like
/// <see cref="FileSecretBackend"/>: every logical key resolves to a distinct
/// Resource, and the namespace is this backend's own.</para>
///
/// <para><b>Failures are swallowed by contract</b>, as on every platform store:
/// the shared arm's write is best-effort and log-only, and a throw here would
/// take down the launch path. One value is capped at 2560 bytes
/// (<c>fauna_credential_store::MAX_ITEM_VALUE_BYTES</c>, Credential Manager's
/// own limit), so a caller that needs a write to have landed reads it back.</para>
/// </summary>
internal sealed class CredManSecretBackend : ISecretBackend
{
    private readonly string _namespace;

    internal CredManSecretBackend(string ns)
    {
        _namespace = ns;
    }

    public string? Get(string resource, string user)
    {
        try
        {
            return FaunaFfiMethods.NativeKeyringGet(_namespace, resource);
        }
        catch (Exception ex)
        {
            ShellLog.Error("SecretStore", $"[CredManSecretBackend] get {resource} failed: {ex.Message}");
            return null;
        }
    }

    public void Set(string resource, string user, string value)
    {
        try
        {
            FaunaFfiMethods.NativeKeyringSet(_namespace, resource, value);
        }
        catch (Exception ex)
        {
            ShellLog.Error("SecretStore", $"[CredManSecretBackend] set {resource} failed: {ex.Message}");
        }
    }

    public void Delete(string resource, string user)
    {
        try
        {
            FaunaFfiMethods.NativeKeyringDelete(_namespace, resource);
        }
        catch (Exception ex)
        {
            ShellLog.Error("SecretStore", $"[CredManSecretBackend] delete {resource} failed: {ex.Message}");
        }
    }
}
