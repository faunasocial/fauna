using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FaunaApp.Controls;

/// <summary>The post image lightbox overlay (<c>image-lightbox</c>) — see the XAML's
/// header for why it is a toggled Button rather than a ContentDialog.</summary>
public sealed partial class ImageLightboxOverlay : UserControl
{
    public ImageLightboxOverlay()
    {
        this.InitializeComponent();
    }

    /// <summary>Open the overlay on an already-loaded bitmap — the card's own, so no
    /// second fetch.</summary>
    public void Show(BitmapImage image)
    {
        LightboxImage.Source = image;
        ImageLightboxDialog.Visibility = Visibility.Visible;
    }

    private void ImageLightboxDialog_Click(object sender, RoutedEventArgs e)
    {
        // Click bubbles from the ScrollViewer/Image too; only dismiss when the
        // ORIGINAL source is the backdrop Button itself.
        if (!ReferenceEquals(e.OriginalSource, sender)) return;
        ImageLightboxDialog.Visibility = Visibility.Collapsed;
        LightboxImage.Source = null;
    }
}
