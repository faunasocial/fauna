using Microsoft.UI.Xaml;
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
            new PropertyMetadata(null));

    public static void SetVisibilityTarget(DependencyObject d, FrameworkElement? value) => d.SetValue(VisibilityTargetProperty, value);
    public static FrameworkElement? GetVisibilityTarget(DependencyObject d) => (FrameworkElement?)d.GetValue(VisibilityTargetProperty);

    // Monotonic counter stored per-Image so stale async completions (after the
    // container recycled onto a new item) can be ignored.
    private static readonly DependencyProperty RequestVersionProperty =
        DependencyProperty.RegisterAttached(
            "RequestVersion", typeof(int), typeof(ImageHashBind),
            new PropertyMetadata(0));

    private static async void OnHashChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is not Image img) return;

        var hash = GetHash(img);
        var loader = GetLoader(img);

        if (string.IsNullOrEmpty(hash) || loader is null)
        {
            img.Source = null;
            SetVisible(img, false);
            SetPaintState(img);
            return;
        }

        // Bump the version so any in-flight load for an older hash no-ops.
        var version = (int)img.GetValue(RequestVersionProperty) + 1;
        img.SetValue(RequestVersionProperty, version);

        // A recycled container may still carry the PREVIOUS item's painted state;
        // drop it to placeholder for the duration of this fetch so a paint read
        // mid-flight never reports a picture that belongs to a different post.
        SetPaintState(img);

        var bmp = await loader.LoadAsync(hash);

        // If the Image recycled to a new hash while we were loading, drop this result.
        if ((int)img.GetValue(RequestVersionProperty) != version) return;

        if (bmp is not null)
        {
            img.Source = bmp;
            SetVisible(img, true);
        }
        else
        {
            img.Source = null;
            SetVisible(img, false);
        }

        // Read the ACTUAL outcome of the assignment above, not which branch ran —
        // a literal "painted" written beside `img.Source = bmp` would still read
        // painted after that one line was deleted.
        SetPaintState(img);
    }

    private static void SetVisible(Image img, bool visible)
    {
        var visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        if (GetManageVisibility(img)) img.Visibility = visibility;
        if (GetVisibilityTarget(img) is { } target) target.Visibility = visibility;
    }

    /// <summary>
    /// Answers <c>get_attr(post-image, "state")</c> (<see cref="ImagePaintState"/>) from
    /// the Image's OWN current <see cref="Image.Source"/>, read back after it was
    /// assigned — never from a literal beside the assignment. Set on
    /// <see cref="VisibilityTargetProperty"/> when one is present (post-image's
    /// AutomationId sits on the wrapping Button, not the Image itself), else on the
    /// Image directly (link-preview-image, media-thumbnail).
    /// </summary>
    private static void SetPaintState(Image img)
    {
        var state = ImagePaintState.From(img.Source is not null);
        FrameworkElement target = GetVisibilityTarget(img) ?? img;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(target, state);
    }
}
