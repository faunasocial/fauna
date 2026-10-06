using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Windows' implementation of the shared multi-account <c>SecretStore</c> seam
/// (<c>FfiSecretStore</c>, a UniFFI callback interface —
/// <c>libs/fauna-ffi/src/accounts_registry.rs</c>).
///
/// <para><b>This is the platform's ONLY foreign seam for identity persistence.</b>
/// Everything above it — the account index, per-actor namespacing, the shared
/// onboarding moments, and the <c>LaunchPersistence</c> the launch machine
/// routes on — is shared Rust
/// (<c>fauna-client-accounts</c>). Windows contributes exactly two things: the
/// key→key table in <see cref="SecretKeyMap"/>, and the raw key/value backend in
/// <see cref="ISecretBackend"/>. Do not add composition logic here — that is
/// what <c>long-term-store.md:348</c> ("the platform SecretStore stays a trivial
/// key→key map") forbids, and it is why CR-3 could fix four apps' slot
/// collision by fixing one crate.</para>
///
/// <para>Linux's twin is <c>fauna_credential_store::CredentialStore</c>: the
/// same shape (resolve the logical key, then hit one of two backends), so the
/// two apps stay comparable line-for-line.</para>
///
/// <para><b>Save failures are swallowed by contract</b> (matching every other
/// platform store, and the shared crate's documented expectation): a store
/// failure means the user re-onboards next launch — the wizard already handles
/// that — whereas throwing here would take down the launch path. Where
/// durability actually matters (the pending-factory-reset claim code, CR-1) the
/// shared <c>mint_and_persist_pending_factory_reset</c> rail reads its own write
/// back and refuses to dispatch if it did not land, so the swallow is safe by
/// construction rather than by hope.</para>
/// </summary>
internal sealed class LogicalSecretStore : FfiSecretStore
{
    private readonly ISecretBackend _backend;

    internal LogicalSecretStore(ISecretBackend backend)
    {
        _backend = backend;
    }

    /// <summary>The backend, for the few call sites that need a raw namespace sweep.</summary>
    internal ISecretBackend Backend => _backend;

    public string? Get(string key)
    {
        var (resource, user) = SecretKeyMap.Resolve(key);
        return _backend.Get(resource, user);
    }

    public void Set(string key, string value)
    {
        var (resource, user) = SecretKeyMap.Resolve(key);
        _backend.Set(resource, user, value);
    }

    public void Delete(string key)
    {
        var (resource, user) = SecretKeyMap.Resolve(key);
        _backend.Delete(resource, user);
    }
}
