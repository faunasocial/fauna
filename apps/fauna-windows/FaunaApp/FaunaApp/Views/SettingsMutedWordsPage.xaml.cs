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
/// AutomationProperties.Name that keeps it in the UIA content view. Each row
/// is the term, its level picker and its remove button (settings.md § Muted
/// words — the level picker).
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

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        _vm?.CleanupStoreChange();
        if (_vm is not null) _vm.StoreResynced -= RenderState;
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.LoadAsync();
        RenderState();
        // The shared store-change notice: another device's word paints on this
        // open page with no re-visit (account-runtime.md § A runtime's own pump
        // is a source of the notice too).
        _vm.StoreResynced -= RenderState;
        _vm.StoreResynced += RenderState;
        _vm.WatchStore();
    }

    private void RenderState()
    {
        if (_vm is null) return;

        ErrorBar.Message = _vm.ErrorMessage ?? string.Empty;
        ErrorBar.IsOpen = !string.IsNullOrEmpty(_vm.ErrorMessage);
        // The page mirror `messages.error` reads (every sibling sub-page's
        // OnPanelError idiom): without it this page's refusal — a read gate's
        // "needs a connection to your nest" — painted the banner yet read back
        // as no error at all.
        App.CurrentErrorMessage = string.IsNullOrEmpty(_vm.ErrorMessage) ? null : _vm.ErrorMessage;

        WordsContainer.Children.Clear();
        foreach (var row in _vm.Rows)
            WordsContainer.Children.Add(BuildWordRow(row));
        // Two conditions, not one: a read has resolved AND it found nothing
        // (docs/goal/ui/README.md § List pages: loading is not empty). The XAML
        // starts EmptyText Collapsed, so a page still loading — or one whose
        // first read FAILED, which is what makes this more than a start-state —
        // paints no empty state beside its error.
        EmptyText.Visibility = _vm.Loaded && _vm.Words.Count == 0
            ? Visibility.Visible
            : Visibility.Collapsed;
        ApplyBusy();
    }

    /// <summary>The level picker, the add button and every remove button are
    /// disabled while any round trip is in flight (settings.md § Muted words —
    /// the level picker). Set on the controls in place rather than by a
    /// re-render: a gesture's handler runs on the very control a rebuild would
    /// tear out from under it.</summary>
    private void ApplyBusy()
    {
        var enabled = _vm is not { Busy: true };
        AddButton.IsEnabled = enabled;
        foreach (var child in WordsContainer.Children)
        {
            if (child is not Panel row) continue;
            foreach (var control in row.Children)
                if (control is Control c) c.IsEnabled = enabled;
        }
    }

    private FrameworkElement BuildWordRow(MutedWordRow entry)
    {
        var word = entry.Word;
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

        row.Children.Add(BuildLevelSelect(entry));

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

    /// <summary>The row's <c>muted-word-level-select</c> (settings.md § Muted
    /// words — the level picker): its options are the shared catalog's
    /// (<c>muted_keyword_levels</c>), each item NAMED by its level's token
    /// (<c>hide</c> | <c>show-less</c>) — the value a driver selects and reads
    /// back — while painting this app's label. A soft row differs from a hidden
    /// one by this value alone. Pre-selected BEFORE the handler attaches, so
    /// painting a row's stored level never fires a save (the FoldersPage
    /// conflict-policy render-echo guard).</summary>
    private ComboBox BuildLevelSelect(MutedWordRow entry)
    {
        var select = new ComboBox
        {
            Tag = entry.Word,
            MinWidth = 140,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(select, Ids.MutedWordLevelSelect);
        foreach (var option in MutedWordsViewModel.LevelOptions())
        {
            var item = new ComboBoxItem { Content = LevelLabel(option.Level), Tag = option.Token };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, option.Token);
            select.Items.Add(item);
            if (option.Token == entry.Level) select.SelectedItem = item;
        }
        select.SelectionChanged += LevelSelect_SelectionChanged;
        return select;
    }

    /// <summary>The level's label — the only part of the option this app owns.</summary>
    private static string LevelLabel(uniffi.fauna_core.MutedKeywordLevel level) =>
        level == uniffi.fauna_core.MutedKeywordLevel.ShowLess
            ? S.Get("muted_words/level_show_less")
            : S.Get("muted_words/level_hide");

    /// <summary>A pick is a save at once, like the remove button; the row
    /// repaints from the record the save returns.</summary>
    private async void LevelSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_vm is null || _vm.Busy) return;
        if (sender is not ComboBox { Tag: string word } select
            || select.SelectedItem is not ComboBoxItem { Tag: string level }) return;
        var save = _vm.SetLevelAsync(word, level);
        // The disable goes on the NEXT dispatcher pass, never inside this
        // handler: a pick made through UI Automation (SelectionItem.Select — a
        // screen reader, the e2e bridge) is still on the stack while
        // SelectionChanged runs, and a picker disabled under it reports
        // ElementNotEnabled for a pick that took (measured 2026-11-04: the save
        // landed, the caller read a failure). Busy is already set, so a second
        // gesture is refused meanwhile.
        DispatcherQueue.TryEnqueue(ApplyBusy);
        await save;
        RenderState();
    }

    private async void AddButton_Click(object sender, RoutedEventArgs e) => await SubmitAddAsync();

    private async void InputBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter) await SubmitAddAsync();
    }

    private async Task SubmitAddAsync()
    {
        if (_vm is null || _vm.Busy) return;
        var term = InputBox.Text;
        var add = _vm.AddAsync(term);
        ApplyBusy();
        if (await add)
            InputBox.Text = string.Empty;
        RenderState();
    }

    private async void RemoveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || _vm.Busy || sender is not Button btn || btn.Tag is not string word) return;
        var remove = _vm.RemoveAsync(word);
        ApplyBusy();
        await remove;
        RenderState();
    }
}
