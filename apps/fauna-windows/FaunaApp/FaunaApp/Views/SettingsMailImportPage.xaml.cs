using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Mail import sub-page (settings.md § Navigation model;
/// mailbox-migration.md § UX shape). Thin host for the self-contained
/// <c>MailImportPanel</c> UserControl — <c>.Configure(clients)</c> + route its machine
/// error up via ErrorChanged to this page's shared <c>error-message</c>. Twin of
/// <see cref="SettingsMailExportPage"/>, the sibling mailbox-portability page it sits
/// directly after in the settings rail.
/// </summary>
public sealed partial class SettingsMailImportPage : Page
{
    public SettingsMailImportPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            MailImportControl.ErrorChanged += OnPanelError;
            MailImportControl.Configure(clients);
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
