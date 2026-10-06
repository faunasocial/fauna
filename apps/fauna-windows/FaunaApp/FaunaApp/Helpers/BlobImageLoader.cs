using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Storage.Streams;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Helpers;

/// <summary>
/// Loads blob images via the authenticated nest API and caches them by hash.
/// WinUI BitmapImage doesn't support auth headers, so we download bytes first.
/// <para><b>The client is resolved per call, never captured.</b> The pages that own a
/// loader are long-lived (<c>FeedPage</c> holds it in a <c>static</c> so the XAML
/// <c>x:Bind</c> has a stable target), but <see cref="INestHttpClient"/> instances are
/// NOT: <c>App.DisposeNestClients</c> disposes and replaces the client on every session
/// or nest-URL change. A loader that held the instance it was constructed with kept
/// calling a disposed <c>HttpClient</c> after any re-login — <c>ObjectDisposedException</c>,
/// swallowed below, so feed and conversation images silently stopped loading until an app
/// restart. Taking a resolver instead of a reference makes that staleness
/// unrepresentable.</para>
/// </summary>
public sealed class BlobImageLoader : IHashImageLoader
{
    private readonly Func<INestHttpClient?> _nest;
    private readonly Func<IFfiFeedManager?>? _feedManager;
    private readonly Dictionary<string, (BitmapImage Image, bool HasC2pa)> _cache = new();

    /// <summary>The manager the cached bitmaps were opened under — see the cache-reset
    /// note in <see cref="LoadWithC2paAsync"/>. Compared by reference, mirroring
    /// <c>FeedManagerHost</c>'s own rebuild key.</summary>
    private IFfiFeedManager? _cacheKeyedTo;

    /// <param name="nest">Resolver for the app's CURRENT nest client (e.g.
    /// <c>() =&gt; App.CurrentNest</c>). Called on every miss — do not close over a
    /// specific <see cref="INestHttpClient"/> instance.</param>
    public BlobImageLoader(Func<INestHttpClient?> nest) : this(nest, null) { }

    /// <param name="nest">As above.</param>
    /// <param name="feedManager">Resolver for the session's feed manager (e.g.
    /// <c>() =&gt; FeedManagerHost.Current</c>), which holds the per-post keys that open a
    /// tier-restricted post's attachments. Resolved per call for the same reason the nest
    /// client is: the manager is rebuilt on re-auth and cleared at an actor change.
    /// <c>null</c> for a loader whose blobs are never post media (the conversations page's
    /// link-preview images), which then keeps the pre-seam behavior exactly.
    /// <para><c>internal</c> because <c>IFfiFeedManager</c> is: the generated UniFFI
    /// surface is internal to <c>FaunaApp.Core</c> and reaches this assembly only through
    /// <c>[InternalsVisibleTo]</c>, so it cannot appear in a public signature.</para></param>
    internal BlobImageLoader(Func<INestHttpClient?> nest, Func<IFfiFeedManager?>? feedManager)
    {
        _nest = nest;
        _feedManager = feedManager;
    }

    public async Task<BitmapImage?> LoadAsync(string hash)
    {
        var result = await LoadWithC2paAsync(hash);
        return result.Image;
    }

    public async Task<(BitmapImage? Image, bool HasC2pa)> LoadWithC2paAsync(string hash)
    {
        var manager = _feedManager?.Invoke();

        // A content address alone stopped being a sufficient cache key once this cache
        // could hold UNSEALED bytes: a tier-restricted post's attachment is opened with a
        // per-post key only this actor's manager holds, so a bitmap decoded for one actor
        // must not survive into the next one's session. The manager instance is exactly
        // that boundary — FeedManagerHost rebuilds it on re-auth and clears it at an actor
        // change — so a change of manager drops the cache. Public blobs are unaffected in
        // substance (they are unauthenticated by design and would re-fetch identically);
        // paying one re-fetch for them is cheaper than reasoning about which cached bitmap
        // came from which key.
        if (!ReferenceEquals(manager, _cacheKeyedTo))
        {
            _cache.Clear();
            _cacheKeyedTo = manager;
        }

        // Hash-keyed within one manager's lifetime: a blob hash is content-addressed, and
        // the cached value is an already-decoded bitmap.
        if (_cache.TryGetValue(hash, out var cached))
            return (cached.Image, cached.HasC2pa);

        var nest = _nest();
        if (nest is null) return (null, false);

        try
        {
            // Fetch, then open: a gated post's attachment is sealed under the same
            // per-post key its body opened under (PostMediaOpen). `null` bytes mean a
            // known-sealed item did not open — degrade to the same collapsed image a
            // decode failure gives, and DON'T cache it, so the render after the post's
            // detail-open unlock retries and paints.
            var (bytes, hasC2pa) = await PostMediaOpen.FetchAndOpenAsync(nest, manager, hash);
            if (bytes is null) return (null, false);
            var bmp = await BitmapFromBytesAsync(bytes);
            if (bmp is null) return (null, false);
            _cache[hash] = (bmp, hasC2pa);
            return (bmp, hasC2pa);
        }
        catch (Exception ex)
        {
            // Swallow-and-skip: a missing image must not take the page down. But it is
            // traced — a silent swallow here is exactly what hid the disposed-client bug
            // above (a broken image looks identical to a post that simply has none).
            System.Diagnostics.Debug.WriteLine($"BlobImageLoader: blob {hash} failed to load: {ex}");
            return (null, false);
        }
    }

    /// <summary>
    /// Decode raw image bytes into a <see cref="BitmapImage"/> via an in-memory
    /// stream (WinUI's <c>BitmapImage</c> takes a stream, not a byte[]). Shared by
    /// the nest-GET path above and the conversations attachment render path
    /// (<c>DmMessageBubble</c>), which resolves bytes from the shared attachment
    /// store rather than the nest. Returns <c>null</c> on a decode failure (e.g.
    /// non-image bytes), mirroring the instance loader's swallow-and-skip.
    /// </summary>
    public static async Task<BitmapImage?> BitmapFromBytesAsync(byte[] bytes)
    {
        try
        {
            var bmp = new BitmapImage();
            using var stream = new InMemoryRandomAccessStream();
            using (var writer = new Windows.Storage.Streams.DataWriter(stream))
            {
                writer.WriteBytes(bytes);
                await writer.StoreAsync();
                writer.DetachStream();
            }
            stream.Seek(0);
            await bmp.SetSourceAsync(stream);
            return bmp;
        }
        catch
        {
            return null;
        }
    }

    public void Clear() => _cache.Clear();
}
