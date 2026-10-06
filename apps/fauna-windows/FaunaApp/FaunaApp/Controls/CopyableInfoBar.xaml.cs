using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;

namespace FaunaApp.Controls;

/// <summary>
/// Error/info bar whose message is selectable and copyable: a copy button sits
/// next to the close (X), and the message renders in a text-selection-enabled
/// TextBlock. Drop-in for the per-page <c>error-message</c> InfoBar — exposes the
/// same <see cref="Message"/> / <see cref="IsOpen"/> / <see cref="Severity"/> /
/// <see cref="IsClosable"/> surface, so a page's existing
/// <c>ErrorBar.Message = …; ErrorBar.IsOpen = …;</c> code-behind is unchanged.
/// Collapses itself when not open (a closed bar takes no layout space).
/// </summary>
public sealed partial class CopyableInfoBar : UserControl
{
    public CopyableInfoBar()
    {
        this.InitializeComponent();
    }

    public string? Message
    {
        get => (string?)GetValue(MessageProperty);
        set => SetValue(MessageProperty, value);
    }

    public static readonly DependencyProperty MessageProperty = DependencyProperty.Register(
        nameof(Message), typeof(string), typeof(CopyableInfoBar), new PropertyMetadata(null));

    public bool IsOpen
    {
        get => (bool)GetValue(IsOpenProperty);
        set => SetValue(IsOpenProperty, value);
    }

    public static readonly DependencyProperty IsOpenProperty = DependencyProperty.Register(
        nameof(IsOpen), typeof(bool), typeof(CopyableInfoBar), new PropertyMetadata(false, OnIsOpenChanged));

    private static void OnIsOpenChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        // Collapse the control when closed so it occupies no layout space (incl.
        // any Margin the host set), matching a bare collapsed InfoBar.
        ((CopyableInfoBar)d).Visibility = (bool)e.NewValue ? Visibility.Visible : Visibility.Collapsed;
    }

    public InfoBarSeverity Severity
    {
        get => (InfoBarSeverity)GetValue(SeverityProperty);
        set => SetValue(SeverityProperty, value);
    }

    public static readonly DependencyProperty SeverityProperty = DependencyProperty.Register(
        nameof(Severity), typeof(InfoBarSeverity), typeof(CopyableInfoBar),
        new PropertyMetadata(InfoBarSeverity.Error));

    public bool IsClosable
    {
        get => (bool)GetValue(IsClosableProperty);
        set => SetValue(IsClosableProperty, value);
    }

    public static readonly DependencyProperty IsClosableProperty = DependencyProperty.Register(
        nameof(IsClosable), typeof(bool), typeof(CopyableInfoBar), new PropertyMetadata(true));

    public string? Title
    {
        get => (string?)GetValue(TitleProperty);
        set => SetValue(TitleProperty, value);
    }

    public static readonly DependencyProperty TitleProperty = DependencyProperty.Register(
        nameof(Title), typeof(string), typeof(CopyableInfoBar), new PropertyMetadata(null));

    private void CopyButton_Click(object sender, RoutedEventArgs e)
    {
        var dp = new DataPackage();
        dp.SetText(Message ?? string.Empty);
        Clipboard.SetContent(dp);
    }
}
