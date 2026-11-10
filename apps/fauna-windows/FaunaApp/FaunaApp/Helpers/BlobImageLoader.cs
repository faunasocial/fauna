using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Storage.Streams;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Helpers;

/// <summary>
/// Loads blob images via the authenticated nest API and caches them by hash — and a
/// bridged post's proxied picture by its nest-relative path (<see cref="LoadProxiedAsync"/>).
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
/// <para><b>One load per picture, however often it is asked for.</b> Both caches are
/// <see cref="SingleFlightCache{TValue}"/>s: a caller that asks while a picture's load is
/// still running shares that load. The feed asks for each picture many times while it
/// loads (every post-card sync requests an unchecked row's C2PA verdict, and a
/// content-changed row re-binds its image), so a cache of finished loads alone re-fetched,
/// re-parsed and re-decoded the same blob on every tick — the UI-thread storm
/// <c>SingleFlightCacheTests</c> pins.</para>
/// </summary>
public sealed class BlobImageLoader : IHashImageLoader, IProxiedImageLoader
{
    private readonly Func<INestHttpClient?> _nest;
    private readonly Func<IFfiFeedManager?>? _feedManager;

    /// <summary>Decoded post pictures and their C2PA verdict, keyed by blob hash. Only a
    /// picture that decoded is kept: a sealed item that did not open, or bytes that will
    /// not decode, is loaded again by the next caller.</summary>
    private readonly SingleFlightCache<(BitmapImage? Image, bool HasC2pa)> _cache =
        new(result => result.Image is not null);

    /// <summary>Decoded bridged-post pictures, keyed by their nest-relative path
    /// (<see cref="LoadProxiedAsync"/>). Dropped with <see cref="_cache"/>.</summary>
    private readonly SingleFlightCache<BitmapImage?> _proxiedCache = new(bmp => bmp is not null);

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

    public Task<BitmapImage?> LoadAsync(string hash) => LoadAsync(hash, null);

    /// <summary>As <see cref="LoadAsync(string)"/>, from the nest the post's own bytes rest
    /// on — <paramref name="home"/> is the post's <c>FfiPostBlobHome</c> (<c>null</c> = this
    /// nest), handed to the shared post-blob plane unchanged.</summary>
    public async Task<BitmapImage?> LoadAsync(string hash, object? home)
    {
        var result = await LoadWithC2paAsync(hash, home);
        return result.Image;
    }

    public Task<(BitmapImage? Image, bool HasC2pa)> LoadWithC2paAsync(string hash, object? home = null)
    {
        var manager = _feedManager?.Invoke();

        // A content address alone stopped being a sufficient cache key once this cache
        // could hold UNSEALED bytes: a tier-restricted post's attachment is opened with a
        // per-post key only this actor's manager holds, so a bitmap decoded for one actor
        // must not survive into the next one's session. The manager instance is exactly
        // that boundary — FeedManagerHost rebuilds it on re-auth and clears it at an actor
        // change — so a change of manager drops the cache, and with it every load still
        // running under the old manager (SingleFlightCache.Clear). Public blobs are
        // unaffected in substance (they are unauthenticated by design and would re-fetch
        // identically); paying one re-fetch for them is cheaper than reasoning about which
        // cached bitmap came from which key.
        DropCachesOnManagerChange(manager);

        // Hash-keyed within one manager's lifetime: a blob hash is content-addressed, and
        // the cached value is an already-decoded bitmap. A caller asking while the hash is
        // loading shares that load (see the class note).
        return _cache.GetOrLoadAsync(hash, () => FetchOpenDecodeAsync(hash, home, manager));
    }

    private async Task<(BitmapImage? Image, bool HasC2pa)> FetchOpenDecodeAsync(
        string hash, object? home, IFfiFeedManager? manager)
    {
        var nest = _nest();
        if (nest is null) return (null, false);

        try
        {
            // Fetch, then open: a gated post's attachment is sealed under the same
            // per-post key its body opened under (PostMediaOpen). `null` bytes mean a
            // known-sealed item did not open — degrade to the same collapsed image a
            // decode failure gives, and the cache does NOT keep it (its keep predicate),
            // so the render after the post's detail-open unlock retries and paints.
            // A foreign home with no plane built yet is a miss, never a fallback to this
            // nest's by-hash GET (that would ask the wrong nest, bearer attached).
            var postHome = home as FfiPostBlobHome;
            var hosts = Core.PostBlobHostsHost.Current;
            if (postHome is not null && hosts is null) return (null, false);
            var (bytes, hasC2pa) = await PostMediaOpen.FetchAndOpenAsync(
                nest, manager, hash, hosts: hosts, home: postHome);
            if (bytes is null) return (null, false);
            var bmp = await BitmapFromBytesAsync(bytes);
            if (bmp is null) return (null, false);
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

    /// <summary>A bridged post's picture (render-model.md § D6c): its bytes, fetched from
    /// the user's own nest by nest-relative <paramref name="path"/> with the session bearer
    /// — the GET a blob takes, with a different argument — and decoded. No
    /// <see cref="PostMediaOpen"/> step and no C2PA verdict: the nest proxies a third
    /// party's public picture, so nothing is sealed and nothing is attested. A fetch that
    /// fails or will not decode is not cached, so the next bind retries; a bind while the
    /// path is loading shares that load.</summary>
    public Task<BitmapImage?> LoadProxiedAsync(string path)
    {
        // The bearer belongs to the session that asked: a new manager is a new session.
        DropCachesOnManagerChange(_feedManager?.Invoke());

        return _proxiedCache.GetOrLoadAsync(path, () => FetchDecodeProxiedAsync(path));
    }

    private async Task<BitmapImage?> FetchDecodeProxiedAsync(string path)
    {
        var nest = _nest();
        if (nest is null) return null;

        try
        {
            var bytes = await nest.GetContentAsync(path);
            return await BitmapFromBytesAsync(bytes);
        }
        catch (Exception ex)
        {
            // Swallow-and-skip, traced — as for a blob above. The placeholder stays.
            System.Diagnostics.Debug.WriteLine($"BlobImageLoader: proxied picture {path} failed to load: {ex}");
            return null;
        }
    }

    private void DropCachesOnManagerChange(IFfiFeedManager? manager)
    {
        if (ReferenceEquals(manager, _cacheKeyedTo)) return;
        _cache.Clear();
        _proxiedCache.Clear();
        _cacheKeyedTo = manager;
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

    public void Clear()
    {
        _cache.Clear();
        _proxiedCache.Clear();
    }
}
