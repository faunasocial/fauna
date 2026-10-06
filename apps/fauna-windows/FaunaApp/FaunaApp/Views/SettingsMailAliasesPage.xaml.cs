using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Mail aliases sub-page (settings.md § Navigation model;
/// mail-aliases.md). Thin host for the self-contained <c>MailAliasesPanel</c>
/// UserControl — <c>.Configure(clients)</c> + route its machine error up via
/// ErrorChanged to this page's shared error-message.
/// </summary>
public sealed partial class SettingsMailAliasesPage : Page
{
    public SettingsMailAliasesPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            MailAliasesControl.ErrorChanged += OnPanelError;
            MailAliasesControl.Configure(clients);
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
