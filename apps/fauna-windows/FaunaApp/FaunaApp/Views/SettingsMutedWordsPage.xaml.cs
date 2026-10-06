using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Muted words sub-page (settings.md § Navigation model, placed
/// right after Privacy; moderation.md § Muted keywords). A person manages
/// their single user-global tier-1 keyword-mute list here — add a term, see
/// the terms, remove one. Structurally the mail-aliases sub-page CRUD list,
/// only simpler (no add-sheet, no kind picker — the input + add-button sit
/// directly on the page, mirroring linux <c>settings/muted_words.rs</c>).
/// Rows are built in code-behind (the BackupsPage <c>RenderDestinations</c>
/// idiom) so each carries the indexed AutomationId + the
/// AutomationProperties.Name that keeps it in the UIA content view.
/// </summary>
public sealed partial class SettingsMutedWordsPage : Page
{
    private MutedWordsViewModel? _vm;

    public SettingsMutedWordsPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients && clients.Rpc is not null)
        {
            _vm = new MutedWordsViewModel(clients.Rpc);
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.LoadAsync();
        RenderState();
    }

    private void RenderState()
    {
        if (_vm is null) return;

        ErrorBar.Message = _vm.ErrorMessage ?? string.Empty;
        ErrorBar.IsOpen = !string.IsNullOrEmpty(_vm.ErrorMessage);

        WordsContainer.Children.Clear();
        foreach (var word in _vm.Words)
            WordsContainer.Children.Add(BuildWordRow(word));
        // Two conditions, not one: a read has resolved AND it found nothing
        // (docs/goal/ui/README.md § List pages: loading is not empty). The XAML
        // starts EmptyText Collapsed, so a page still loading — or one whose
        // first read FAILED, which is what makes this more than a start-state —
        // paints no empty state beside its error.
        EmptyText.Visibility = _vm.Loaded && _vm.Words.Count == 0
            ? Visibility.Visible
            : Visibility.Collapsed;
    }

    private FrameworkElement BuildWordRow(string word)
    {
        var row = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 12,
            Padding = new Thickness(8),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.MutedWordItem);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, word);

        var text = new TextBlock { Text = word, VerticalAlignment = VerticalAlignment.Center };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(text, Ids.MutedWordText);
        row.Children.Add(text);

        var remove = new Button
        {
            Content = S.Get("muted_words/remove"),
            Tag = word,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(remove, Ids.MutedWordRemoveButton);
        remove.Click += RemoveButton_Click;
        row.Children.Add(remove);

        return row;
    }

    private async void AddButton_Click(object sender, RoutedEventArgs e) => await SubmitAddAsync();

    private async void InputBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter) await SubmitAddAsync();
    }

    private async Task SubmitAddAsync()
    {
        if (_vm is null) return;
        var term = InputBox.Text;
        if (await _vm.AddAsync(term))
            InputBox.Text = string.Empty;
        RenderState();
    }

    private async void RemoveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button btn || btn.Tag is not string word) return;
        await _vm.RemoveAsync(word);
        RenderState();
    }
}
