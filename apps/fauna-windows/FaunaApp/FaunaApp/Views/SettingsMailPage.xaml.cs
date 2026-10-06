using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Mail sub-page (settings.md § Navigation model; mail-settings.md).
/// Thin host for the self-contained <c>MailSettingsPanel</c> UserControl —
/// <c>.Configure(clients)</c> + route its machine error up via ErrorChanged to
/// this page's shared error-message, exactly as the former single-scroll
/// SettingsPage embedded it.
/// </summary>
public sealed partial class SettingsMailPage : Page
{
    public SettingsMailPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            MailSettingsControl.ErrorChanged += OnPanelError;
            MailSettingsControl.Configure(clients);
        }
    }

    private void OnPanelError(string? message)
    {
        if (string.IsNullOrEmpty(message))
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
        else
        {
            ErrorBar.Message = message;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = message;
        }
    }

    private void Page_Loaded(object sender, RoutedEventArgs e)
    {
        // The hosted panel loads itself on its own Loaded; nothing else here.
    }
}
