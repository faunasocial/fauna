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
}
