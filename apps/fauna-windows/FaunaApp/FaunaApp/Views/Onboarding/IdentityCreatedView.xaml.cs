using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

public sealed partial class IdentityCreatedView : UserControl
{
    public IdentityCreatedView()
    {
        InitializeComponent();
    }

    internal IdentityCreatedView(OnboardingViewModel vm) : this()
    {
        DataContext = vm;
    }

    private void SecretKeyCopyBtn_Click(object sender, RoutedEventArgs e) =>
        FaunaApp.Helpers.ClipboardHelper.CopyText(SecretKeyText.Text);
}
