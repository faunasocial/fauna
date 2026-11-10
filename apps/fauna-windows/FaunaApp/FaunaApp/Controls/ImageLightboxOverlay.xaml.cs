using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FaunaApp.Controls;

/// <summary>The image lightbox overlay (<c>image-lightbox</c>) — see the XAML's
/// header for why it is a toggled Button rather than a ContentDialog.</summary>
public sealed partial class ImageLightboxOverlay : UserControl
{
    public ImageLightboxOverlay()
    {
        this.InitializeComponent();
    }

    /// <summary>Whether the overlay is showing — a host closes it when the surface it
    /// opened over goes away (the conversations page, on leaving the thread).</summary>
    public bool IsOpen => ImageLightboxDialog.Visibility == Visibility.Visible;

    /// <summary>Open the overlay on an already-loaded bitmap — the card's or the
    /// bubble's own, so no second fetch.</summary>
    public void Show(BitmapImage image)
    {
        LightboxPlaceholder.Visibility = Visibility.Collapsed;
        LightboxPlaceholder.Text = string.Empty;
        LightboxImage.Source = image;
        LightboxImage.Visibility = Visibility.Visible;
        Open();
    }

    /// <summary>Open the overlay on a picture whose bytes the device does not hold: its
    /// declared placeholder (<c>{filename} ({size})</c>, the text the bubble paints in
    /// the picture's place), never a dropped click (conversations.md § User actions,
    /// the <c>dm-attachment-image</c> row).</summary>
    public void ShowPlaceholder(string declared)
    {
        LightboxImage.Source = null;
        LightboxImage.Visibility = Visibility.Collapsed;
        LightboxPlaceholder.Text = declared;
        LightboxPlaceholder.Visibility = Visibility.Visible;
        Open();
    }

    /// <summary>Close the overlay, dropping the picture it held.</summary>
    public void Hide()
    {
        ImageLightboxDialog.Visibility = Visibility.Collapsed;
        LightboxImage.Source = null;
        LightboxPlaceholder.Text = string.Empty;
    }

    /// <summary>Show the overlay and take keyboard focus, so Escape — the lightbox's
    /// one dismiss key, since ui.yaml declares no dismiss element inside — reaches
    /// <see cref="ImageLightboxDialog_KeyDown"/>. The first attempt can land before
    /// the just-shown Button is laid out; one retry after layout then takes it.</summary>
    private void Open()
    {
        ImageLightboxDialog.Visibility = Visibility.Visible;
        if (!ImageLightboxDialog.Focus(FocusState.Programmatic))
        {
            DispatcherQueue.TryEnqueue(
                Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
                () => { if (IsOpen) ImageLightboxDialog.Focus(FocusState.Programmatic); });
        }
    }

    private void ImageLightboxDialog_Click(object sender, RoutedEventArgs e)
    {
        // Click bubbles from the ScrollViewer/Image too; only dismiss when the
        // ORIGINAL source is the backdrop Button itself.
        if (!ReferenceEquals(e.OriginalSource, sender)) return;
        Hide();
    }

    /// <summary>Escape closes the lightbox and leaves the page beneath it as it was —
    /// the overlay key shape the conversations page's own overlays use.</summary>
    private void ImageLightboxDialog_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != Windows.System.VirtualKey.Escape) return;
        Hide();
        e.Handled = true;
    }
}
