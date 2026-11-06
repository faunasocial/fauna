using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Controls;

/// <summary>
/// The by-continuing terms notice on the account-making onboarding pages
/// (invite_request, claim_code, nest_provisioning, and handle_entry while
/// Continue signs in). The link opens the shared terms URL through the one
/// <see cref="FaunaApp.Services.UrlOpener"/>.
/// </summary>
public sealed partial class TermsNotice : UserControl
{
    public TermsNotice() { InitializeComponent(); }

    private void Link_Click(object sender, RoutedEventArgs e)
        => FaunaApp.Services.UrlOpener.Open(
            uniffi.fauna_ffi.FaunaFfiMethods.FaunaFfiTermsUrl(), "Onboarding");
}
