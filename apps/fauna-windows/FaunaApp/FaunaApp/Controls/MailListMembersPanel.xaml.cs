using System;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// User-facing "List members" surface (docs/goal/behavior/mail-mass-mailing.md
/// § mail-list-members page): managing the members of one mailing list — summary, add a
/// member, batch-import, per-member unsubscribe / resubscribe. A thin view over
/// <see cref="MailListMembersViewModel"/> (FaunaApp.Core), which projects the shared
/// <c>fauna_client_mail_settings::MailListMembersMachine</c> (scoped to one list) through
/// its UniFFI <c>IMailListMembersMachine</c> interface (no business logic here, priority #2).
/// Hosted by its dedicated Settings shell sub-page <c>SettingsMailListMembersPage</c>, which
/// supplies <c>ServiceClients</c> via <c>OnNavigatedTo</c> and surfaces this panel's
/// <see cref="ErrorChanged"/> on its own page-level <c>error-message</c>. Lifts
/// apps/fauna-linux/src/settings/mail_list_members.rs.
///
/// In the full app this is opened either scoped to the list whose Members button was
/// clicked, or — a direct settings-rail visit, or the embedded settings seed (no
/// inter-page routing) — falls back to the caller's first owned list, or the honest
/// empty-state placeholder id when there are none (mail-mass-mailing.md § Per-app render
/// status — tui's shape, not linux's; linux stays placeholder-only for this case).
/// </summary>
public sealed partial class MailListMembersPanel : UserControl
{
    /// <summary>Placeholder list id for the embedded seed AND for a direct rail-nav visit
    /// when the caller genuinely has no lists (16 zero bytes = a valid hex id the machine
    /// accepts) — the honest "no such list" empty state, not a crash. A visit with nothing
    /// selected but at least one owned list instead falls back to the first one
    /// (mail-mass-mailing.md § Per-app render status — the tui shape), resolved in
    /// <see cref="EnsureVmAsync"/>.</summary>
    private const string PlaceholderListIdHex = "00000000000000000000000000000000";

    /// <summary>The list to scope to on the NEXT construction — set by
    /// <see cref="MailListsPanel.Members_Click"/> just before navigating here, consumed
    /// (and cleared) once in <see cref="EnsureVmAsync"/>. <c>null</c> when this page was
    /// reached via the settings rail directly rather than a row's Members button.</summary>
    internal static string? PendingListIdHex { get; set; }

    private ServiceClients? _clients;
    private MailListMembersViewModel? _vm;

    public event Action<string?>? ErrorChanged;

    public MailListMembersPanel()
    {
        this.InitializeComponent();
    }

    internal void Configure(ServiceClients clients) => _clients = clients;

    private async void Panel_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private async Task EnsureVmAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // Scoped to the list whose Members button was clicked (consumed once — the NEXT
        // construction, e.g. a later rail visit with nothing newly selected, re-resolves
        // below). User-class machine — needs no secret / node_url; built over the
        // session's shared, auto-reconnecting WS-RPC connection (the INestRpcClient seam)
        // rather than a per-panel one-shot FfiNestClient.Connect().
        string listId;
        string listName;
        if (PendingListIdHex is { } pending)
        {
            listId = pending;
            listName = S.Get("mail_lists/members_title");
        }
        else
        {
            // A direct rail visit with nothing selected falls back to the caller's
            // first owned list (mail-mass-mailing.md § Per-app render status — the
            // tui shape). Unlike tui, this panel has no already-hydrated
            // MailListsMachine to peek at (each settings panel builds its own machine
            // independently), so the fallback needs its own "list my lists, take the
            // first" read before the scoped machine can be constructed.
            var listsVm = new MailListsViewModel(await _clients.Rpc.BuildMailListsMachineAsync());
            await listsVm.LoadCommand.ExecuteAsync(null);
            var first = listsVm.Lists.FirstOrDefault();
            // No owned lists at all: the honest empty state, not a fabricated id.
            listId = first?.ListIdHex ?? PlaceholderListIdHex;
            listName = first?.FriendlyName ?? S.Get("mail_lists/members_title");
        }
        PendingListIdHex = null;
        var machine = await _clients.Rpc.BuildMailListMembersMachineAsync(listId, listName);
        _vm = new MailListMembersViewModel(machine);
        MembersList.ItemsSource = _vm.Members;
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
        ErrorChanged?.Invoke(string.IsNullOrEmpty(_vm.Error) ? null : _vm.Error);
        SummaryText.Text = _vm.Summary;
        // Loading is not empty (ui/README.md § List pages: loading is not empty) — a
        // pre-hydrate empty Members must not flash "no list open" before the first load
        // (real or placeholder-id) completes.
        ListEmpty.Visibility = _vm.Loaded && _vm.Members.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Add-member sheet ──
    private void AddMember_Click(object sender, RoutedEventArgs e)
    {
        ImportSheet.Visibility = Visibility.Collapsed;
        AddressInput.Text = string.Empty;
        AddSheet.Visibility = Visibility.Visible;
    }

    private async void AddSubmit_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var address = AddressInput.Text.Trim();
        if (string.IsNullOrEmpty(address)) return;
        await _vm.AddMemberAsync(address);
        RenderState();
        if (string.IsNullOrEmpty(_vm.Error)) AddSheet.Visibility = Visibility.Collapsed;
    }

    private void AddCancel_Click(object sender, RoutedEventArgs e)
        => AddSheet.Visibility = Visibility.Collapsed;

    // ── Import sheet ──
    private void Import_Click(object sender, RoutedEventArgs e)
    {
        AddSheet.Visibility = Visibility.Collapsed;
        ImportInput.Text = string.Empty;
        ImportSheet.Visibility = Visibility.Visible;
    }

    private async void ImportSubmit_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.BatchImportAsync(ImportInput.Text);
        RenderState();
        if (string.IsNullOrEmpty(_vm.Error)) ImportSheet.Visibility = Visibility.Collapsed;
    }

    private void ImportCancel_Click(object sender, RoutedEventArgs e)
        => ImportSheet.Visibility = Visibility.Collapsed;

    // ── Per-member subscribe-state controls ──
    private async void Unsubscribe_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not string address || _vm is null) return;
        await _vm.UnsubscribeAsync(address);
        RenderState();
    }

    private async void Resubscribe_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not string address || _vm is null) return;
        await _vm.ResubscribeAsync(address);
        RenderState();
    }
}
