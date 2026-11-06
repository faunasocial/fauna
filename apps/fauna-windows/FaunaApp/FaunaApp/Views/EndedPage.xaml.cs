using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Views;

/// <summary>
/// The empty page a shell's inner frame is left on once the shell itself has been
/// navigated away from — what <see cref="NestedFrame.EndPage"/> navigates to so the page
/// that WAS in the frame gets its own <c>OnNavigatedFrom</c>. Nothing is ever shown on
/// it: the shell holding the frame is already leaving the screen. A XAML page (not a
/// bare <see cref="Page"/>) so the frame can construct it through the app's own XAML
/// type information.
/// </summary>
public sealed partial class EndedPage : Page
{
    public EndedPage()
    {
        this.InitializeComponent();
    }
}
