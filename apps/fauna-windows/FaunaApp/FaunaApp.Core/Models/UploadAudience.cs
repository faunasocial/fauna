namespace FaunaApp.Core.Models;

/// <summary>
/// The audience a blob upload is sealed under — the windows-uniform selector the
/// upload path (<see cref="Services.INestHttpClient.UploadBlobAsync"/>) maps to the
/// shared-Rust <c>FfiUploadAudience</c> (<c>process_and_seal</c>) before POSTing the
/// sealed bytes + DAG-CBOR sidecar as <c>multipart/form-data</c> to
/// <c>/api/v1/blob</c>.
/// <para>Goal: <c>docs/goal/architecture/encryption-at-rest.md</c> § Per-content-kind
/// conformance → Media row. (Wire design tracked internally.)</para>
/// <para>Only the two <em>client-key</em> audiences are expressible: the MLS-keyed
/// audiences (Conversation, RestrictedPost) must not extract their epoch/period
/// secret across the FFI boundary — they get a shared-Rust seal-by-id helper
/// (tracked internally) and are deliberately absent here.</para>
/// </summary>
public abstract record UploadAudience
{
    private UploadAudience() { }

    /// <summary>
    /// Public-post-attached media — no seal; the bytes pass through as signed
    /// plaintext (the post's signature attests the blob hash).
    /// </summary>
    public sealed record PublicPost : UploadAudience;

    /// <summary>
    /// Owner-only library media, sealed under the owner's 32-byte
    /// <c>BackupKey</c> (derived from the identity seed via
    /// <c>BackupKey::derive</c>).
    /// </summary>
    public sealed record Library(byte[] BackupKey) : UploadAudience;
}
