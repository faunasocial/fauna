using System.Threading.Tasks;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FaunaApp.Helpers;

/// <summary>
/// Resolves a bridged post's picture — a nest-relative proxied path, the
/// <c>ProxiedImage</c> block's address (<c>docs/goal/architecture/render-model.md</c>
/// § D6c) — to a decoded <see cref="BitmapImage"/> for an <see cref="ImageHashBind"/>-bound
/// <see cref="Microsoft.UI.Xaml.Controls.Image"/>. A separate face from
/// <see cref="IHashImageLoader"/> because a path is not a content hash: the bytes come
/// from the same bearer-carrying GET, but there is nothing to open and no C2PA verdict.
/// Returns <c>null</c> on a miss / decode failure, which leaves the placeholder standing.
/// </summary>
public interface IProxiedImageLoader
{
    Task<BitmapImage?> LoadProxiedAsync(string path);
}
