using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Media;

namespace FaunaApp.Helpers;

/// <summary>
/// Attached properties that make an <see cref="Image"/> load a blob by hash through
/// a <see cref="BlobImageLoader"/>. Mirrors how web uses a plain &lt;img src&gt; and
/// Apple uses AsyncImage: virtualized ListView containers only fire a load for
/// realized rows, and the loader's cache dedups repeat hits.
///
/// Usage:
///   &lt;Image helpers:ImageHashBind.Hash="{x:Bind MediaHashHex, Mode=OneWay}"
///          helpers:ImageHashBind.Loader="{x:Bind local:FeedPage.GetImageLoader(), Mode=OneTime}" /&gt;
///
/// <para>A bridged post's picture has no hash: it is a nest-relative path
/// (<see cref="ProxiedPathProperty"/>, render-model.md § D6c), loaded through the same
/// loader's <see cref="IProxiedImageLoader"/> face. Unlike a blob image, which shows only
/// once it has loaded, a proxied one holds its slot from the first frame behind a
/// placeholder (<see cref="PlaceholderProperty"/>) whose automation text is the path.</para>
/// </summary>
public static class ImageHashBind
{
    public static readonly DependencyProperty HashProperty =
        DependencyProperty.RegisterAttached(
            "Hash", typeof(string), typeof(ImageHashBind),
            new PropertyMetadata(null, OnHashChanged));

    public static void SetHash(DependencyObject d, string? value) => d.SetValue(HashProperty, value);
    public static string? GetHash(DependencyObject d) => (string?)d.GetValue(HashProperty);

    public static readonly DependencyProperty LoaderProperty =
        DependencyProperty.RegisterAttached(
            "Loader", typeof(IHashImageLoader), typeof(ImageHashBind),
            new PropertyMetadata(null, OnHashChanged));

    public static void SetLoader(DependencyObject d, IHashImageLoader? value) => d.SetValue(LoaderProperty, value);
    public static IHashImageLoader? GetLoader(DependencyObject d) => (IHashImageLoader?)d.GetValue(LoaderProperty);

    // A bridged post's picture: the nest-relative path of its `ProxiedImage` block
    // (render-model.md § D6c). Used only while Hash is empty — a blob image wins the
    // slot — and only with a Loader that is also an IProxiedImageLoader.
    public static readonly DependencyProperty ProxiedPathProperty =
        DependencyProperty.RegisterAttached(
            "ProxiedPath", typeof(string), typeof(ImageHashBind),
            new PropertyMetadata(null, OnHashChanged));

    public static void SetProxiedPath(DependencyObject d, string? value) => d.SetValue(ProxiedPathProperty, value);
    public static string? GetProxiedPath(DependencyObject d) => (string?)d.GetValue(ProxiedPathProperty);

    // When false, the caller owns the Image's Visibility and this binding only sets the
    // Source. Used by the link-preview og:image, whose presence is gated on the post's
    // remote-content reveal (render-model.md § D4) — NOT on whether the blob loaded: a
    // failed tier_2 load (no real nest blob) must leave the revealed element realized so the
    // e2e can assert its presence (a real blob paints it in tier_3+/prod). Default true
    // preserves the post-image behavior (show on successful load, hide otherwise).
    public static readonly DependencyProperty ManageVisibilityProperty =
        DependencyProperty.RegisterAttached(
            "ManageVisibility", typeof(bool), typeof(ImageHashBind),
            new PropertyMetadata(true));

    public static void SetManageVisibility(DependencyObject d, bool value) => d.SetValue(ManageVisibilityProperty, value);
    public static bool GetManageVisibility(DependencyObject d) => (bool)d.GetValue(ManageVisibilityProperty);

    // An ancestor (e.g. a Button wrapping this Image so the click target carries a
    // real UIA InvokePattern — feed post-image/image-lightbox) that should track the
    // SAME load-completion signal as the Image's own Visibility, independent of
    // ManageVisibility: gating a wrapping control's presence on the hash alone (known
    // synchronously, before the async fetch resolves) would let automation act on it
    // before the image is actually loaded. Null (default) is a no-op.
    public static readonly DependencyProperty VisibilityTargetProperty =
        DependencyProperty.RegisterAttached(
            "VisibilityTarget", typeof(FrameworkElement), typeof(ImageHashBind),
            new PropertyMetadata(null, OnPresentationTargetChanged));

    public static void SetVisibilityTarget(DependencyObject d, FrameworkElement? value) => d.SetValue(VisibilityTargetProperty, value);
    public static FrameworkElement? GetVisibilityTarget(DependencyObject d) => (FrameworkElement?)d.GetValue(VisibilityTargetProperty);

    // The element that stands in for a proxied picture until its bytes are on screen —
    // still loading, or a fetch that failed (the next bind retries). Shown exactly while
    // the placeholder text is (ImagePaintState.PlaceholderText); never for a blob image.
    // Null (default) is a no-op.
    public static readonly DependencyProperty PlaceholderProperty =
        DependencyProperty.RegisterAttached(
            "Placeholder", typeof(FrameworkElement), typeof(ImageHashBind),
            new PropertyMetadata(null, OnPresentationTargetChanged));

    public static void SetPlaceholder(DependencyObject d, FrameworkElement? value) => d.SetValue(PlaceholderProperty, value);
    public static FrameworkElement? GetPlaceholder(DependencyObject d) => (FrameworkElement?)d.GetValue(PlaceholderProperty);

    // Monotonic counter stored per-Image so stale async completions (after the
    // container recycled onto a new item) can be ignored.
    private static readonly DependencyProperty RequestVersionProperty =
        DependencyProperty.RegisterAttached(
            "RequestVersion", typeof(int), typeof(ImageHashBind),
            new PropertyMetadata(0));

    // What this Image is loading or showing (an ImageSourceKey) — null while it has
    // nothing to paint. Typed object: the key is a private managed type, not a WinRT one.
    private static readonly DependencyProperty ActiveSourceProperty =
        DependencyProperty.RegisterAttached(
            "ActiveSource", typeof(object), typeof(ImageHashBind),
            new PropertyMetadata(null));

    /// <summary>One picture to load: a blob by content hash, or a bridged post's picture
    /// by nest-relative path. A record, so two binds that resolve to the same picture
    /// through the same loader compare equal.</summary>
    private sealed record ImageSourceKey(bool Proxied, string Key, IHashImageLoader Loader);

    private static ImageSourceKey? Resolve(Image img)
    {
        if (GetLoader(img) is not { } loader) return null;
        if (GetHash(img) is { Length: > 0 } hash) return new ImageSourceKey(false, hash, loader);
        if (GetProxiedPath(img) is { Length: > 0 } path && loader is IProxiedImageLoader)
            return new ImageSourceKey(true, path, loader);
        return null;
    }

    private static async void OnHashChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is not Image img) return;

        var source = Resolve(img);

        if (source is null)
        {
            // Bump the version so an in-flight load for the previous picture no-ops.
            img.SetValue(RequestVersionProperty, (int)img.GetValue(RequestVersionProperty) + 1);
            img.SetValue(ActiveSourceProperty, null);
            img.Source = null;
            Present(img);
            return;
        }

        // Three properties feed one picture, so a bind that leaves it unchanged (an empty
        // ProxiedPath arriving beside a hash that is already loading) must not fetch again.
        if (source.Equals(img.GetValue(ActiveSourceProperty))) return;
        img.SetValue(ActiveSourceProperty, source);

        // Bump the version so any in-flight load for an older picture no-ops.
        var version = (int)img.GetValue(RequestVersionProperty) + 1;
        img.SetValue(RequestVersionProperty, version);

        // A proxied picture's placeholder stands in from the first frame, so a recycled
        // container must not keep showing the PREVIOUS item's picture under it.
        if (source.Proxied) img.Source = null;

        // A recycled container may still carry the PREVIOUS item's painted state;
        // drop it to placeholder for the duration of this fetch so a paint read
        // mid-flight never reports a picture that belongs to a different post.
        Present(img);

        var bmp = source.Proxied
            ? await ((IProxiedImageLoader)source.Loader).LoadProxiedAsync(source.Key)
            : await source.Loader.LoadAsync(source.Key);

        // If the Image recycled to a new picture while we were loading, drop this result.
        if ((int)img.GetValue(RequestVersionProperty) != version) return;

        img.Source = bmp;

        // Read the ACTUAL outcome of the assignment above, not which branch ran —
        // a literal "painted" written beside `img.Source = bmp` would still read
        // painted after that one line was deleted.
        Present(img);
    }

    // VisibilityTarget and Placeholder are bound beside Hash / ProxiedPath / Loader, in
    // an order x:Bind decides; whichever lands last, the elements they name must show
    // the state the load has already reached.
    private static void OnPresentationTargetChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is Image img) Present(img);
    }

    /// <summary>
    /// Brings everything the Image's paint drives into line with its OWN current
    /// <see cref="Image.Source"/> and active source — never with which branch of the load
    /// ran:
    /// <list type="bullet">
    /// <item>visibility (the Image when <see cref="ManageVisibilityProperty"/>, and the
    /// <see cref="VisibilityTargetProperty"/>): shown once a picture is on screen, and
    /// for a proxied picture from the first frame, behind its placeholder;</item>
    /// <item><c>get_attr(post-image, "state")</c> (<see cref="ImagePaintState"/>), as
    /// <c>AutomationProperties.HelpText</c> on the <see cref="VisibilityTargetProperty"/>
    /// when one is present (post-image's AutomationId sits on the wrapping Button, not
    /// the Image itself), else on the Image directly (link-preview-image,
    /// media-thumbnail);</item>
    /// <item>the <see cref="PlaceholderProperty"/> element and the same element's
    /// automation Name — a proxied picture's path while its placeholder stands in
    /// (<see cref="ImagePaintState.PlaceholderText"/>), cleared once it has painted. The
    /// Name is touched only on an Image that binds <see cref="ProxiedPathProperty"/>, so
    /// a Name another call site set in XAML is left alone.</item>
    /// </list>
    /// </summary>
    private static void Present(Image img)
    {
        var painted = img.Source is not null;
        var proxiedPath = img.GetValue(ActiveSourceProperty) is ImageSourceKey { Proxied: true } active
            ? active.Key
            : null;
        FrameworkElement target = GetVisibilityTarget(img) ?? img;

        var visibility = painted || proxiedPath is not null ? Visibility.Visible : Visibility.Collapsed;
        if (GetManageVisibility(img)) img.Visibility = visibility;
        if (GetVisibilityTarget(img) is { } visibilityTarget) visibilityTarget.Visibility = visibility;

        AutomationProperties.SetHelpText(target, ImagePaintState.From(painted));

        var placeholderText = ImagePaintState.PlaceholderText(proxiedPath, painted);
        if (GetPlaceholder(img) is { } placeholder)
            placeholder.Visibility = placeholderText.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        if (img.ReadLocalValue(ProxiedPathProperty) != DependencyProperty.UnsetValue)
        {
            if (placeholderText.Length > 0)
                AutomationProperties.SetName(target, placeholderText);
            else
                target.ClearValue(AutomationProperties.NameProperty);
        }
    }
}
