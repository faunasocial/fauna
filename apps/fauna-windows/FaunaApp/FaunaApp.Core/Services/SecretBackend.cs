namespace FaunaApp.Core.Services;

/// <summary>
/// The raw key/value store the logical-keyed <see cref="LogicalSecretStore"/>
/// sits on. The app's one implementation is <see cref="SharedSecretBackend"/>:
/// the shared Rust credential store over the FFI, which picks Credential
/// Manager or the harness's file itself, exactly as it does for linux's
/// <c>fauna_credential_store::CredentialStore</c>. Unit tests supply in-memory
/// implementations.
///
/// <para>Keys are the registry's logical keys, verbatim — <see cref="LogicalSecretStore"/>
/// passes each one straight through (android's <c>SecretBackend</c> is the twin).</para>
/// </summary>
internal interface ISecretBackend
{
    string? Get(string resource);
    void Set(string resource, string value);
    void Delete(string resource);
}
