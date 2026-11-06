using System.Threading.Tasks;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FaunaApp.Helpers;

/// <summary>
/// Resolves a content-hash to a decoded <see cref="BitmapImage"/> for an
/// <see cref="ImageHashBind"/>-bound <see cref="Microsoft.UI.Xaml.Controls.Image"/>.
/// Two implementations share the one binding mechanism (priority #2/#4):
/// <see cref="BlobImageLoader"/> — an authenticated by-hash blob GET (feed /
/// conversation images) — and <see cref="MediaThumbnailLoader"/> — the Media
/// explorer's owner-sealed thumbnails, fetched + decrypted through the shared-Rust
/// <c>MediaMachine::fetch_thumbnail</c>. Returns <c>null</c> on a miss / decode
/// failure so the binding leaves the item's placeholder (never blanks the page).
/// </summary>
public interface IHashImageLoader
{
    Task<BitmapImage?> LoadAsync(string hash);

    /// <summary>
    /// Load <paramref name="hash"/> from the nest the post's own bytes rest on —
    /// <paramref name="home"/> is the post's shared <c>post_blob_home</c>, passed through
    /// UNCHANGED (typed <c>object</c> because the generated record is internal to
    /// <c>FaunaApp.Core</c> and a public signature cannot name it). <c>null</c> = this
    /// nest. A loader whose blobs are never a post's own bytes ignores it.
    /// </summary>
    Task<BitmapImage?> LoadAsync(string hash, object? home) => LoadAsync(hash);
}
