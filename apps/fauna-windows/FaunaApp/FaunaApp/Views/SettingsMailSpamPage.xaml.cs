using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Mail spam sub-page (settings.md § Navigation model; mail-spam.md).
/// Thin host for the self-contained <c>MailSpamPanel</c> UserControl —
/// <c>.Configure(clients)</c> + route its machine error up via ErrorChanged to
/// this page's shared error-message.
/// </summary>
public sealed partial class SettingsMailSpamPage : Page
{
    public SettingsMailSpamPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            MailSpamControl.ErrorChanged += OnPanelError;
            MailSpamControl.Configure(clients);
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
    }
}
