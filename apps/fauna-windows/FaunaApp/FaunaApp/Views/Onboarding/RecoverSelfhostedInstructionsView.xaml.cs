using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Box-recovery step-4 self-hosted install page (Task E): shows the installer
/// command carrying the recovery seed. Mirrors
/// <c>apps/fauna-linux/src/views/onboarding/recover_selfhosted_instructions.rs</c>
/// + the web reference. The command is a placeholder until the C2 reachable-nest
/// render lands (box-recovery.md § Implementation status of step 4); copy is a
/// pure clipboard action, and both restore/continue leave the recovery flow via
/// the machine's <c>reset()</c> (mirroring linux). Per
/// <c>docs/goal/architecture/nest/box-recovery.md</c> § Recovery UI (step 4).
/// </summary>
public sealed partial class RecoverSelfhostedInstructionsView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    public RecoverSelfhostedInstructionsView()
    {
        InitializeComponent();
    }

    internal RecoverSelfhostedInstructionsView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
        ViewModel.PropertyChanged += (_, _) => Bindings.Update();
    }

    /// <summary>Copy the installer command to the clipboard — no machine call
    /// (mirrors linux's GDK-clipboard copy).</summary>
    private void OnCopyClick(object sender, RoutedEventArgs e)
    {
        var pkg = new DataPackage();
        pkg.SetText(ViewModel.RecoverSelfhostedCommand);
        Clipboard.SetContent(pkg);
    }
}
