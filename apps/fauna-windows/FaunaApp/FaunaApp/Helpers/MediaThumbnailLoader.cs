using System.Collections.Generic;
using System.Threading.Tasks;
using Microsoft.UI.Xaml.Media.Imaging;
using uniffi.fauna_media_machine;

namespace FaunaApp.Helpers;

/// <summary>
/// Resolves a Media-explorer item's <c>thumbnail_hash</c> to a decoded thumbnail via
/// the shared-Rust <c>MediaMachine::fetch_thumbnail</c> (media.md § Thumbnails / §
/// Where logic lives): a direct-by-hash blob GET → content-address verify → decrypt
/// under the owner <c>BackupKey</c> (Library audience) → decoded JPEG bytes, all in
/// shared Rust; the client only paints the bytes (priority #2). Distinct from
/// <see cref="BlobImageLoader"/> (a plain authenticated blob GET) because a Media
/// thumbnail is owner-sealed and has no plaintext <c>/api/v1/blob</c> primary — so it
/// is fetched direct-by-hash, NOT via <c>?thumb=1</c> (media.md § Thumbnails).
///
/// A per-hash cache dedups repeat hits (virtualized rows re-realize). A fetch/decode
/// failure returns <c>null</c> so <see cref="ImageHashBind"/> keeps the placeholder
/// for that one item without blanking the page (media.md § Errors — per-item, never
/// the page error banner).
/// </summary>
// Internal because the ctor takes the UniFFI-generated MediaMachine, which
// uniffi-bindgen-cs emits as `internal` — a public member of an internal type is a
// CS0051 accessibility error. GetThumbLoader() exposes it to XAML as the public
// IHashImageLoader interface instead.
internal sealed class MediaThumbnailLoader : IHashImageLoader
{
    private readonly MediaMachine _machine;
    private readonly byte[] _backupKey;
    private readonly Dictionary<string, BitmapImage> _cache = new();

    internal MediaThumbnailLoader(MediaMachine machine, byte[] backupKey)
    {
        _machine = machine;
        _backupKey = backupKey;
    }

    public async Task<BitmapImage?> LoadAsync(string hash)
    {
        if (_cache.TryGetValue(hash, out var cached))
            return cached;

        try
        {
            // Shared Rust does the fetch + content-address verify + BackupKey decrypt;
            // the returned bytes are ALREADY-decoded JPEG — paint directly (do NOT
            // decrypt again, do NOT use ?thumb=1). media.md § Thumbnails.
            var bytes = await _machine.FetchThumbnail(hash, _backupKey);
            var bmp = await BlobImageLoader.BitmapFromBytesAsync(bytes);
            if (bmp is null) return null;
            _cache[hash] = bmp;
            return bmp;
        }
        catch
        {
            // Per-item degrade: a missing/undecodable thumbnail keeps the placeholder.
            return null;
        }
    }
}
