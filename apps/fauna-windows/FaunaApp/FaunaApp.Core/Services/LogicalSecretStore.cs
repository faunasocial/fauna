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
/// (<c>fauna-client-accounts</c>). Windows contributes exactly one thing: the raw
/// key/value backend in <see cref="ISecretBackend"/>, which receives every logical
/// key verbatim (android's <c>LogicalSecretStore</c> is the twin). Do not add
/// composition logic here — that is
/// what <c>long-term-store.md:348</c> ("the platform SecretStore stays a trivial
/// key→key map") forbids, and it is why CR-3 could fix four apps' slot
/// collision by fixing one crate.</para>
///
/// <para>Linux's twin is <c>fauna_credential_store::CredentialStore</c>, which the
/// backend here reaches over the FFI: the same logical key lands on the same row
/// on both apps.</para>
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

    public string? Get(string key) => _backend.Get(key);

    public void Set(string key, string value) => _backend.Set(key, value);

    public void Delete(string key) => _backend.Delete(key);
}
