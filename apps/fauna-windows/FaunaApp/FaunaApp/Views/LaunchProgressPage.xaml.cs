using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;

namespace FaunaApp.Views;

/// <summary>
/// <c>launch_progress</c> — the surface the root frame shows while
/// <c>LaunchMachine.Start()</c> runs the launch's sign-in check, before
/// <c>App.xaml.cs:DispatchLaunchSnapshotAsync</c> routes on its result. It carries
/// one element, <c>launch-identity-label</c>: the bound (else active) account's
/// cached <c>handle@domain · tier</c>, composed in shared Rust
/// (<c>FfiAccountRegistry.LaunchIdentityLine</c> →
/// <c>fauna_client_accounts::identity_line</c>), and is blank — the label absent,
/// not empty — while the cache holds no handle (<c>onboarding.md</c> § App-launch
/// routing → <i>The cached identity on the launch surfaces</i>). The navigation
/// parameter is that line, or null. Android's <c>LaunchProgressScreen</c>, linux's
/// Launching phase and FaunaKit's <c>LaunchProgressView</c> are the twins.
/// </summary>
public sealed partial class LaunchProgressPage : Page
{
    public LaunchProgressPage()
    {
        InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        LaunchIdentityLabel.Paint(IdentityText, e.Parameter as string);
    }
}

/// <summary>
/// Paints <c>launch-identity-label</c> on the two launch surfaces that carry it,
/// <see cref="LaunchProgressPage"/> and <see cref="LaunchRetryPage"/>: the line, or
/// the element collapsed out of the tree when there is none. (Not named
/// <c>LaunchIdentity</c>: that is the launch machine's own record type.)
/// </summary>
internal static class LaunchIdentityLabel
{
    internal static void Paint(TextBlock label, string? line)
    {
        label.Text = line ?? "";
        label.Visibility = string.IsNullOrEmpty(line)
            ? Microsoft.UI.Xaml.Visibility.Collapsed
            : Microsoft.UI.Xaml.Visibility.Visible;
    }
}
