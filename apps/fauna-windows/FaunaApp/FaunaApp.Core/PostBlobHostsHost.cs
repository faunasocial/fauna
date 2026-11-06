using System.Threading;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core;

/// <summary>
/// The ONE <see cref="FfiPostBlobHosts"/> for the current session — the shared
/// post-blob plane (<c>fauna_client::post_blob</c>) every post byte leg rides: the
/// gated body, media, thumbnails and the C2PA probe (<c>feed.md</c> § The read model →
/// <i>A post homed on another nest</i>; <c>transport-trust.md</c> § The post arm).
///
/// <para><b>Per session, because the foreign pins live as long as the handle:</b> the
/// plane graduates a foreign home's pin once for this handle's life, so a handle built
/// per image would re-graduate on every card. Rebuilt when the transport it was built
/// over is a different object (re-auth), cleared at an actor change — the keying
/// <see cref="FeedManagerHost"/> uses, for the same reason.</para>
///
/// <para>The app never picks the host: it hands each post's
/// <c>post_blob_home</c> back with the hash, unchanged.</para>
/// </summary>
internal static class PostBlobHostsHost
{
    private static readonly SemaphoreSlim Gate = new(1, 1);

    private static FfiPostBlobHosts? _current;

    private static object? _builtOver;

    /// <summary>The session's plane, or <c>null</c> before any page built one and after an
    /// actor change. Readers only — the build goes through <see cref="GetOrBuildAsync"/>.</summary>
    public static FfiPostBlobHosts? Current => _current;

    /// <summary>
    /// The session's plane, built on first use over <paramref name="rpc"/> and reused by
    /// every later page; rebuilt when <paramref name="rpc"/> is a different object than the
    /// cached handle was built over.
    /// </summary>
    public static async Task<FfiPostBlobHosts> GetOrBuildAsync(Services.INestRpcClient rpc)
    {
        if (_current is { } live && ReferenceEquals(_builtOver, rpc)) return live;
        await Gate.WaitAsync();
        try
        {
            if (_current is { } raced && ReferenceEquals(_builtOver, rpc)) return raced;
            var built = await rpc.BuildPostBlobHostsAsync();
            _current = built;
            _builtOver = rpc;
            return built;
        }
        finally
        {
            Gate.Release();
        }
    }

    /// <summary>End the outgoing identity's plane at an actor change (the twin of
    /// <see cref="FeedManagerHost.ResetForActorChange"/>; called from
    /// <c>App.DropActorScopedState</c>).</summary>
    public static void ResetForActorChange()
    {
        _current = null;
        _builtOver = null;
    }
}
