using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

public sealed partial class DnsPostInstructionsView : UserControl
{
    // Typed VM property used by x:Bind on the read-only TextBox. Defaulted
    // to null! since the parameterless ctor (used by the XAML loader's
    // design-time path) never gets a VM.
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    public DnsPostInstructionsView()
    {
        InitializeComponent();
    }

    internal DnsPostInstructionsView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
    }

    /// <summary>
    /// Copies the DNS instructions text to the system clipboard. Lives in
    /// the codebehind (not the cross-platform VM) because it uses the
    /// Windows-specific <c>Windows.ApplicationModel.DataTransfer</c> APIs
    /// that FaunaApp.Core (net10.0) can't reference.
    /// </summary>
    private void OnCopyClick(object sender, RoutedEventArgs e) =>
        FaunaApp.Helpers.ClipboardHelper.CopyText(ViewModel?.DnsPostInstructions);
}
