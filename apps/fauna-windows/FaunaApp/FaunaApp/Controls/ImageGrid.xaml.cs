using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
using System.Collections.Generic;

namespace FaunaApp.Controls;

public sealed partial class ImageGrid : UserControl
{
    public event Action<BitmapImage>? ImageClicked;

    public ImageGrid()
    {
        this.InitializeComponent();
    }

    public void SetImages(IReadOnlyList<BitmapImage> images)
    {
        GridContainer.Children.Clear();
        if (images.Count == 0) { Visibility = Visibility.Collapsed; return; }
        Visibility = Visibility.Visible;

        for (int i = 0; i < Math.Min(images.Count, 4); i++)
        {
            var img = new Image
            {
                Source = images[i],
                Stretch = Microsoft.UI.Xaml.Media.Stretch.UniformToFill,
                MaxHeight = images.Count == 1 ? 400 : 200,
            };
            var captured = images[i];
            img.Tapped += (s, e) => ImageClicked?.Invoke(captured);

            int row = i / 2;
            int col = i % 2;

            if (images.Count == 1) { Grid.SetColumnSpan(img, 2); }
            else if (images.Count == 3 && i == 2) { Grid.SetColumnSpan(img, 2); }

            Grid.SetRow(img, row);
            Grid.SetColumn(img, col);
            GridContainer.Children.Add(img);
        }
    }
}
