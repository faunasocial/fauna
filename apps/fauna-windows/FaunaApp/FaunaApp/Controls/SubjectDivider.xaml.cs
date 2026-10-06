using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Controls;

public sealed partial class SubjectDivider : UserControl
{
    public static readonly DependencyProperty SubjectProperty =
        DependencyProperty.Register(
            nameof(Subject),
            typeof(string),
            typeof(SubjectDivider),
            new PropertyMetadata("", OnSubjectChanged));

    public string Subject
    {
        get => (string)GetValue(SubjectProperty);
        set => SetValue(SubjectProperty, value);
    }

    public SubjectDivider() { InitializeComponent(); }

    private static void OnSubjectChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is SubjectDivider sd)
        {
            sd.SubjectText.Text = (string)e.NewValue;
        }
    }
}
