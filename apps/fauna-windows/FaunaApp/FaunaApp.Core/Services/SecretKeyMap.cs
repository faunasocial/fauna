namespace FaunaApp.Core.Services;

/// <summary>
/// The logical-key → native-key map for the multi-account registry's
/// <c>SecretStore</c> seam (<c>docs/goal/architecture/long-term-store.md</c>
/// § Multi-account evolution → Shared seam).
///
/// <para>The shared registry addresses every value by a <b>logical</b> key —
/// <c>fauna/index</c>, <c>fauna/{actor_id}/{slot}</c>, <c>install/…</c> — and
/// each is stored <b>verbatim</b>: the logical key becomes the Credential
/// Manager resource, under one shared username. No backend reads the username:
/// the Credential Manager and file backends key by Resource alone.
/// The linux twin is <c>fauna_credential_store::account_for</c>.</para>
///
/// <para>This map used to also carry the pre-registry single-identity rows
/// (<c>legacy/*</c> → <c>FaunaIdentity</c>, <c>FaunaNestUrl</c>, …). They are
/// retired (<c>long-term-store.md</c> § Downgrade mirror + abandoned-append
/// recovery, 2026-09-24): windows reads and writes the registry alone. The
/// shared registry's transitional legacy single-slot flag, which
/// could still write a <c>legacy/*</c> key through this seam, was deleted
/// 2026-09-28 when android, the last app on it, moved off.</para>
///
/// <para>This map is the platform's <b>entire</b> contribution: all composition
/// lives in <c>fauna-client-accounts</c> (<c>long-term-store.md</c> § Shared
/// seam — the platform SecretStore stays a trivial key→key map).</para>
/// </summary>
internal static class SecretKeyMap
{
    /// <summary>
    /// The username shared by every logical key (the key itself is the
    /// Resource). No backend reads it.
    /// </summary>
    internal const string VerbatimUser = "fauna";

    /// <summary>Resolve a registry logical key to its native Credential Manager (Resource, UserName).</summary>
    internal static (string Resource, string User) Resolve(string logicalKey) => (logicalKey, VerbatimUser);
}
