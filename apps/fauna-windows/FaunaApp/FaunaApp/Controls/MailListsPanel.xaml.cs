using System;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Views;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// User-facing "Lists" surface (docs/goal/behavior/mail-mass-mailing.md § mail-lists page
/// UX): a person runs their own mailing lists — create / edit / delete a list on one of
/// their owned domains. A thin view over <see cref="MailListsViewModel"/> (FaunaApp.Core),
/// which projects the shared <c>fauna_client_mail_settings::MailListsMachine</c> through
/// its UniFFI <c>IMailListsMachine</c> interface (no business logic here, priority #2).
/// Builds the machine over the session's shared, auto-reconnecting WS-RPC connection
/// (the INestRpcClient seam). Hosted by its dedicated Settings shell sub-page
/// <c>SettingsMailListsPage</c>, which supplies
/// <c>ServiceClients</c> via <c>OnNavigatedTo</c> and surfaces this panel's
/// <see cref="ErrorChanged"/> on its own page-level <c>error-message</c>.
/// The add/edit sheet + per-row delete confirm are inline reveals. Lifts
/// apps/fauna-linux/src/settings/mail_lists.rs.
/// </summary>
public sealed partial class MailListsPanel : UserControl
{
    private enum FormMode { Add, Edit }

    private ServiceClients? _clients;
    private MailListsViewModel? _vm;

    private FormMode _formMode = FormMode.Add;
    private string? _editingId;
    // The list id armed for the two-click delete confirm.
    private string? _armedDeleteId;

    public event Action<string?>? ErrorChanged;

    public MailListsPanel()
    {
        this.InitializeComponent();
    }

    internal void Configure(ServiceClients clients) => _clients = clients;

    private async void Panel_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private async Task EnsureVmAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // User-class machine — needs no secret / node_url; built over the session's
        // shared, auto-reconnecting WS-RPC connection (the INestRpcClient seam) rather
        // than a per-panel one-shot FfiNestClient.Connect().
        _vm = new MailListsViewModel(await _clients.Rpc.BuildMailListsMachineAsync());
        ListsList.ItemsSource = _vm.Lists;
        DomainPicker.ItemsSource = _vm.LocalDomains;
    }

    private async Task LoadAsync()
    {
        if (_clients is null) return;
        LoadingRing.IsActive = true;
        LoadingRing.Visibility = Visibility.Visible;
        try
        {
            await EnsureVmAsync();
            await _vm!.LoadCommand.ExecuteAsync(null);
            RenderState();
        }
        catch (Exception ex)
        {
            ErrorChanged?.Invoke(Strings.Error(ex));
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
        }
    }

    private void RenderState()
    {
        if (_vm is null) return;

        // Create needs an owned mail domain; without one say why, unless a machine error
        // is already showing (mirrors linux NO_DOMAIN).
        AddButton.IsEnabled = _vm.CanManage;
        var msg = !string.IsNullOrEmpty(_vm.Error)
            ? _vm.Error
            : (!_vm.CanManage ? S.Get("mail_lists/no_domain") : null);
        ErrorChanged?.Invoke(msg);

        // Loading is not empty (ui/README.md § List pages: loading is not empty) — a
        // pre-hydrate empty Lists must not flash "no lists" before the first load completes.
        ListEmpty.Visibility = _vm.Loaded && _vm.Lists.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Add / edit sheet ──

    private void Add_Click(object sender, RoutedEventArgs e) => OpenSheet(FormMode.Add, null);

    /// <summary>Open the add/edit sheet. <paramref name="row"/> null = Add mode (empty);
    /// non-null = Edit mode pre-populated (address — local-part + domain — is immutable on
    /// edit, so those fields are disabled).</summary>
    private void OpenSheet(FormMode mode, MailListRow? row)
    {
        _formMode = mode;
        _editingId = row?.ListIdHex;
        NameInput.Text = row?.FriendlyName ?? string.Empty;
        LocalPartInput.Text = row?.LocalPart ?? string.Empty;
        LocalPartInput.IsEnabled = mode == FormMode.Add;
        DomainPicker.IsEnabled = mode == FormMode.Add;
        DomainPicker.SelectedItem = row?.LocalDomain;
        if (mode == FormMode.Add && DomainPicker.SelectedIndex < 0 && _vm!.LocalDomains.Count > 0)
        {
            DomainPicker.SelectedIndex = 0;
        }
        DescriptionInput.Text = row?.Description ?? string.Empty;
        ListHelpInput.Text = row?.ListHelpUrl ?? string.Empty;
        ListArchiveInput.Text = row?.ListArchiveUrl ?? string.Empty;
        PerSendInput.Text = row?.RecipientsPerSend?.ToString() ?? string.Empty;
        ErrorChanged?.Invoke(null);
        AddSheet.Visibility = Visibility.Visible;
    }

    private async void Submit_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        var friendlyName = NameInput.Text.Trim();
        var localPart = LocalPartInput.Text.Trim();
        var localDomain = DomainPicker.SelectedItem as string ?? string.Empty;
        var description = DescriptionInput.Text.Trim();
        var listHelp = ListHelpInput.Text.Trim();
        var listArchive = ListArchiveInput.Text.Trim();
        var perSend = FaunaFfiMethods.ParseCount(PerSendInput.Text);

        if (_formMode == FormMode.Add)
        {
            await _vm.CreateAsync(friendlyName, localPart, localDomain, description, listHelp, listArchive, perSend);
        }
        else
        {
            if (_editingId is null) return;
            await _vm.UpdateAsync(_editingId, friendlyName, localPart, localDomain, description, listHelp, listArchive, perSend);
        }

        RenderState();
        // Close only on success; on error leave the sheet open for retry.
        if (string.IsNullOrEmpty(_vm.Error))
        {
            AddSheet.Visibility = Visibility.Collapsed;
        }
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        AddSheet.Visibility = Visibility.Collapsed;
        RenderState();
    }

    private void Edit_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not string listId || _vm is null) return;
        var row = _vm.Lists.FirstOrDefault(l => l.ListIdHex == listId);
        if (row is null) return;
        OpenSheet(FormMode.Edit, row);
    }

    /// <summary>Open the mail-list-members settings sub-page scoped to THIS row's list
    /// (mail-mass-mailing.md: "the row's button scopes the page to that list_id", the
    /// tui/linux shape). Stashes the target on <see cref="MailListMembersPanel"/>'s
    /// pending-scope handle — the settings shell's inner Frame has no per-navigation
    /// parameter slot beyond the shared <c>ServiceClients</c> — then routes there via
    /// the shared settings-shell navigator, exactly like
    /// <c>PersonalizationPage</c>'s muted-words/labeler-catalog jumps.</summary>
    private void Members_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not string listId) return;
        MailListMembersPanel.PendingListIdHex = listId;
        MainPage.Current?.NavigateToSettingsSubPage(SettingsNavigation.MailListMembers);
    }

    /// <summary>Delete is destructive — arm-then-confirm (mirrors the linux wire_two_click /
    /// MailAliasesPanel): first click relabels "Confirm?" + auto-disarms after 4 s; the
    /// second click within the window dispatches.</summary>
    private async void Delete_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not string listId || _vm is null) return;

        if (_armedDeleteId != listId)
        {
            _armedDeleteId = listId;
            var baseLabel = btn.Content;
            btn.Content = "Confirm?";
            await Task.Delay(4000);
            if (_armedDeleteId == listId)
            {
                _armedDeleteId = null;
                btn.Content = baseLabel;
            }
            return;
        }

        _armedDeleteId = null;
        await _vm.DeleteAsync(listId);
        RenderState();
    }
}
