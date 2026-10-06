using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

public sealed partial class IdentityImportView : UserControl
{
    public IdentityImportView()
    {
        InitializeComponent();
    }

    internal IdentityImportView(OnboardingViewModel vm) : this()
    {
        DataContext = vm;
    }
}
