using Xunit;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Tests;

/// <summary>
/// The Source step's per-provider field table (<c>mailbox-migration.md</c> § Wizard
/// steps step 1; § Microsoft sign-in — the device-code grant): Outlook is the address
/// and Sign in with Microsoft alone — no password, no host fields, no Connect button.
/// </summary>
public class MailImportSourceFieldsTests
{
    private const int Gmail = 0, Outlook = 1, ICloud = 2, Generic = 3;

    [Fact]
    public void Outlook_shows_the_sign_in_button_and_no_password_host_or_connect()
    {
        var f = MailImportViewModel.FieldsFor(Outlook);
        Assert.True(f.ShowOauthButton);
        Assert.False(f.ShowAppPassword);
        Assert.False(f.ShowImapFields);
        Assert.False(f.ShowConnectButton);
    }

    [Theory]
    [InlineData(Gmail)]
    [InlineData(ICloud)]
    public void App_password_providers_show_the_app_password_and_connect(int kind)
    {
        var f = MailImportViewModel.FieldsFor(kind);
        Assert.True(f.ShowAppPassword);
        Assert.True(f.ShowConnectButton);
        Assert.False(f.ShowOauthButton);
        Assert.False(f.ShowImapFields);
    }

    [Fact]
    public void Generic_shows_the_imap_fields_and_connect_but_no_oauth()
    {
        var f = MailImportViewModel.FieldsFor(Generic);
        Assert.True(f.ShowImapFields);
        Assert.True(f.ShowConnectButton);
        Assert.False(f.ShowOauthButton);
        Assert.False(f.ShowAppPassword);
    }
}
