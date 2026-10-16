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
            // A change made from low on the page (the serve-here switch) leaves this
            // bar scrolled off the top: bring it into view so the refusal is seen
            // (and is IsOffscreen=false to the e2e driver, reference_windows_e2e_is_visible_offscreen).
            ErrorBar.UpdateLayout();
            ErrorBar.StartBringIntoView();
        }
    }

    private void Page_Loaded(object sender, RoutedEventArgs e)
    {
        // The hosted panel loads itself on its own Loaded; nothing else here.
    }
}
