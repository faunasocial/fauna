using System;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// User-facing "Aliases" surface (docs/goal/behavior/mail-aliases.md § Aliases UX):
/// a person manages their own per-account mail addresses — the canonical
/// <handle>@<domain> exact alias, extra exact aliases, a wildcard prefix, and
/// one-click disposable mints, each with an optional label + per-alias controls +
/// disable / revoke / delete. A thin view over <see cref="MailAliasesViewModel"/>
/// (FaunaApp.Core) — which projects the shared
/// <c>fauna_client_mail_settings::MailAliasesMachine</c> through its UniFFI
/// <c>IMailAliasesMachine</c> interface (no business logic here, priority #2). Builds
/// the machine over the session's shared, auto-reconnecting WS-RPC connection (the
/// INestRpcClient seam) and hands it to the VM. Hosted by its
/// dedicated Settings shell sub-page <c>SettingsMailAliasesPage</c>, which supplies
/// <c>ServiceClients</c> via <c>OnNavigatedTo</c> and surfaces this panel's
/// <see cref="ErrorChanged"/> on its own page-level <c>error-message</c>. The add/edit sheet + the
/// per-row destructive confirms are inline reveals (not ContentDialogs) so every ID
/// lives in one tree. The VM's render-state is reflected imperatively (mirrors
/// AdminNestPage); only the alias list is a bound ObservableCollection.
/// </summary>
public sealed partial class MailAliasesPanel : UserControl
{
    /// <summary>Which dispatch the add/edit sheet's submit maps to.</summary>
    private enum FormMode { Add, Edit }

    private ServiceClients? _clients;
    private MailAliasesViewModel? _vm;

    private FormMode _formMode = FormMode.Add;
    // Some(hex) while the sheet is editing that row (Edit mode).
    private string? _editingId;
    // Armed (aliasIdHex, kind) for the two-click destructive confirm; kind ∈ {revoke, delete}.
    private (string Id, string Kind)? _armed;

    /// <summary>Raised with the VM's error message (or null to clear) so the host page
    /// surfaces it through its own page-level <c>error-message</c> element.</summary>
    public event Action<string?>? ErrorChanged;

    public MailAliasesPanel()
    {
        this.InitializeComponent();
    }

    /// <summary>Supplied by the host page (SettingsMailAliasesPage.OnNavigatedTo) before Loaded
    /// fires — carries the secrets used to build the WS client.</summary>
    internal void Configure(ServiceClients clients) => _clients = clients;

    private async void Panel_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private async Task EnsureVmAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // The aliases machine is User-class (the nest derives the owning actor from the
        // authenticated caller); built over the session's shared, auto-reconnecting
        // WS-RPC connection (the INestRpcClient seam) rather than a per-panel one-shot
        // FfiNestClient.Connect().
        _vm = new MailAliasesViewModel(await _clients.Rpc.BuildMailAliasesMachineAsync());
        AliasesList.ItemsSource = _vm.Aliases;
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

    /// <summary>Reflect the VM's scalar state into the UI (the alias list is a bound
    /// ObservableCollection, so it tracks the VM automatically). Called after the load +
    /// every action.</summary>
    private void RenderState()
    {
        if (_vm is null) return;

        // Create/generate need a canonical exact alias (default_domain); without one say
        // why (mail-aliases.md § Kind 1; mirrors linux), unless a machine error is showing.
        AddButton.IsEnabled = _vm.CanManage;
        GenerateButton.IsEnabled = _vm.CanManage;
        ImportButton.IsEnabled = _vm.CanManage;
        var msg = !string.IsNullOrEmpty(_vm.Error)
            ? _vm.Error
            : (!_vm.CanManage ? S.Get("mail_aliases/no_default_domain") : null);
        ErrorChanged?.Invoke(msg);

        // Disposable-mint success: copy the full address + show a transient toast (cleared
        // on the next dispatch, when the machine clears last_minted_address).
        if (!string.IsNullOrEmpty(_vm.LastMintedAddress))
        {
            FaunaApp.Helpers.ClipboardHelper.CopyText(_vm.LastMintedAddress);
            // The button carries a `copied` attr holding the exact string put on the
            // clipboard (the account-actor-id-copy-btn contract; windows get_attr maps a
            // non-`disabled` name to HelpText). Written AFTER the copy from the same value.
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(GenerateButton, _vm.LastMintedAddress);
            MintedToast.Text = $"{S.Get("mail_aliases/copied")} {_vm.LastMintedAddress}";
            MintedToast.Visibility = Visibility.Visible;
        }
        else
        {
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(GenerateButton, "");
            MintedToast.Visibility = Visibility.Collapsed;
        }

        // Loading is not empty (ui/README.md § List pages: loading is not empty) — a
        // pre-hydrate empty Aliases must not flash "no aliases" before the first load
        // completes.
        ListEmpty.Visibility = _vm.Loaded && _vm.Aliases.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

        // Bulk-import result summary (mail-aliases-import-result): the shared
        // mail_aliases.import_result template's placeholders map created→Created,
        // existed→SkippedDuplicate, invalid→Invalid (windows resw is flat — substituted
        // here, mirrors AdminMailPage's PublishSpamBaselineResult .Replace chain). Per
        // mail-aliases.md § Bulk import ("with the reason for each invalid line") and
        // ui.yaml, one import_invalid_line row follows the summary per Invalid outcome —
        // a best-effort import's whole point is that one typo can't block the other 99,
        // so the user must learn *which* line was bad. Same element, no new ui.yaml ID.
        var import = _vm.LastImportResult;
        if (import is null)
        {
            ImportResultText.Text = string.Empty;
            // Collapsed, not merely empty text: an always-visible-but-blank
            // TextBlock reports IsOffscreen=false to FlaUI from the moment the
            // sheet opens, so an e2e `wait_for`-then-`get_text` poll (the
            // sane pattern for "wait until the async result renders") returns
            // immediately with an empty string instead of actually waiting. Mirrors linux's
            // `import_result.set_visible(false)` (settings/mail_aliases.rs).
            // The Visibility toggle lives on the wrapping ImportResultButton, not
            // this TextBlock — see the XAML comment.
            ImportResultButton.Visibility = Visibility.Collapsed;
        }
        else
        {
            var summary = S.Get("mail_aliases/import_result")
                .Replace("{created}", import.Created.ToString())
                .Replace("{existed}", import.SkippedDuplicate.ToString())
                .Replace("{invalid}", import.Invalid.ToString());
            var invalidLines = import.Outcomes
                .Where(o => o.Status == "Invalid")
                .Select(o => S.Get("mail_aliases/import_invalid_line")
                    .Replace("{address}", o.Address)
                    .Replace("{reason}", o.Reason ?? string.Empty));
            ImportResultText.Text = string.Join("\n", new[] { summary }.Concat(invalidLines));
            ImportResultButton.Visibility = Visibility.Visible;
            // The import sheet is taller than the e2e viewport once the result
            // renders, so a just-revealed element can be in the UIA tree but
            // OFFSCREEN — is_visible is !IsOffscreen
            // (reference_windows_e2e_is_visible_offscreen; mirrors
            // PersonalizationPage.BringIntoViewAfterLayout /
            // AtprotoPage.BringIntoViewAfterLayout). UpdateLayout() must run
            // BEFORE StartBringIntoView() — called in the same pass as the
            // Visibility flip above it is a silent no-op, since the button has
            // not been measured yet.
            ImportResultButton.UpdateLayout();
            ImportResultButton.StartBringIntoView();
            DispatcherQueue?.TryEnqueue(
                Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
                () => ImportResultButton.StartBringIntoView());
        }
    }

    // ── Add / edit sheet ──

    private void Add_Click(object sender, RoutedEventArgs e) => OpenSheet(FormMode.Add, null);

    /// <summary>Open the add/edit sheet. <paramref name="row"/> null = Add mode (empty,
    /// exact); non-null = Edit mode pre-populated from that row (kind immutable → the
    /// picker is read-only).</summary>
    private void OpenSheet(FormMode mode, MailAliasRow? row)
    {
        _formMode = mode;
        _editingId = row?.AliasIdHex;
        KindPicker.IsChecked = row?.IsWildcard ?? false;
        KindPicker.IsEnabled = mode == FormMode.Add;
        PatternInput.Text = row?.Pattern ?? string.Empty;
        LabelInput.Text = row?.Label ?? string.Empty;
        SpamThresholdInput.Text = row?.SpamThresholdOverride?.ToString() ?? string.Empty;
        RatePerHourInput.Text = row?.RateLimitPerHour?.ToString() ?? string.Empty;
        SheetTitle.Text = S.Get("mail_aliases/form_title");
        SubmitButton.Content = S.Get("mail_aliases/submit");
        ErrorChanged?.Invoke(null);
        AddSheet.Visibility = Visibility.Visible;
    }

    private async void GenerateDisposable_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.GenerateDisposableAsync();
        RenderState();
    }

    private async void Submit_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        var pattern = PatternInput.Text.Trim();
        var label = LabelInput.Text.Trim();
        var spam = FaunaFfiMethods.ParseCount(SpamThresholdInput.Text);
        var rate = FaunaFfiMethods.ParseCountI64(RatePerHourInput.Text);

        if (_formMode == FormMode.Add)
        {
            await _vm.CreateAsync(KindPicker.IsChecked == true, pattern, label, spam, rate);
        }
        else
        {
            if (_editingId is null) return;
            await _vm.UpdateAsync(_editingId, pattern, label, spam, rate);
        }

        RenderState();
        // Close the sheet only on success; on error RenderState surfaced the error and we
        // leave the sheet open for retry.
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

    // ── Bulk-import sheet (mail-aliases.md § Bulk import) ──

    /// <summary>Open the import sheet — collapses the add/edit sheet first (mirrors
    /// how Edit_Click reuses the single add-sheet region; the two sheets never show
    /// together).</summary>
    private void Import_Click(object sender, RoutedEventArgs e)
    {
        AddSheet.Visibility = Visibility.Collapsed;
        ImportTextArea.Text = string.Empty;
        ImportResultText.Text = string.Empty;
        ImportResultButton.Visibility = Visibility.Collapsed;
        ErrorChanged?.Invoke(null);
        ImportSheet.Visibility = Visibility.Visible;
    }

    private async void ImportSubmit_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        // Split on '\r' AND '\n': a WinUI multi-line TextBox set via UIA
        // ValuePattern.SetValue (the FlaUI bridge's TrySetValueAppend — the
        // physical-typing fallback sends real Enter keystrokes instead) stores
        // line breaks as lone '\r', not '\n' — confirmed by the e2e's 3-line
        // paste splitting into a single line here. '\r\n' produces one empty
        // entry between the two chars, dropped by the blank-line filter below.
        var lines = ImportTextArea.Text
            .Split('\r', '\n')
            .Select(l => l.Trim())
            .Where(l => l.Length > 0)
            .ToList();

        await _vm.ImportAsync(lines);

        RenderState();
        // The sheet stays open on submit, success or failure: `mail-aliases-import-result`
        // lives INSIDE ImportSheet (ui.yaml's `mail-aliases-import-sheet` component), and
        // WinUI drops a Visibility.Collapsed subtree out of the UIA tree entirely — closing
        // on success would hide the very result FlaUI is asked to read. Cancel is the only
        // way to close (mirrors linux `submit_import` / `mail-aliases.md` § Bulk import).
    }

    private void ImportCancel_Click(object sender, RoutedEventArgs e)
    {
        ImportSheet.Visibility = Visibility.Collapsed;
        RenderState();
    }

    private void Edit_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not string aliasId || _vm is null) return;
        var row = _vm.Aliases.FirstOrDefault(a => a.AliasIdHex == aliasId);
        if (row is null) return;
        OpenSheet(FormMode.Edit, row);
    }

    // ── Per-row destructive controls ──

    /// <summary>The two-way "Active" toggle (mail-aliases.md § Disable; linux
    /// <c>mail_aliases.rs</c> is the reference). <c>IsOn</c> = Active (= !disabled):
    /// turning it OFF revokes (soft-off), turning it back ON re-enables — disable is no
    /// longer a one-way trap (a product invariant). The realization echo (x:Bind sets
    /// <c>IsOn</c> from the bound <c>Active</c> when the row realizes) is filtered by
    /// comparing the toggle's new state to the row's current active state: equal means
    /// the bind-driven realize, not a user action, so no-op.</summary>
    private async void DisabledToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (sender is not ToggleSwitch sw || sw.Tag is not string aliasId || _vm is null) return;
        var row = _vm.Aliases.FirstOrDefault(a => a.AliasIdHex == aliasId);
        if (row is null) return;
        var active = !row.Disabled;
        if (sw.IsOn == active) return;            // realization echo (matches row state) — no-op
        if (sw.IsOn) await _vm.EnableAsync(aliasId);  // OFF→ON: re-enable
        else await _vm.RevokeAsync(aliasId);          // ON→OFF: soft-off
        RenderState();
    }

    private async void Revoke_Click(object sender, RoutedEventArgs e) =>
        await ArmOrConfirm(sender, "revoke");

    private async void Delete_Click(object sender, RoutedEventArgs e) =>
        await ArmOrConfirm(sender, "delete");

    /// <summary>Arm-then-confirm a destructive per-row button (no modal, no separate
    /// confirm ID): the first click arms (relabels "Confirm?") and auto-disarms after 4 s;
    /// the second click within the window dispatches. Mirrors the linux <c>wire_two_click</c>
    /// and MailSettingsPanel's revoke confirm.</summary>
    private async Task ArmOrConfirm(object sender, string kind)
    {
        if (sender is not Button btn || btn.Tag is not string aliasId || _vm is null) return;

        if (_armed != (aliasId, kind))
        {
            _armed = (aliasId, kind);
            var baseLabel = btn.Content;
            btn.Content = "Confirm?";
            await Task.Delay(4000);
            if (_armed == (aliasId, kind))
            {
                _armed = null;
                btn.Content = baseLabel;
            }
            return;
        }

        _armed = null;
        if (kind == "revoke")
        {
            await _vm.RevokeAsync(aliasId);
        }
        else
        {
            await _vm.DeleteAsync(aliasId);
        }
        RenderState();
    }

    // ── Helpers ──

    /// <summary>x:Bind visibility helper (the established FeedPage / AdminUsersPage
    /// pattern — a single static call is x:Bind-legal). Visible when <paramref name="value"/>.</summary>
    public static Visibility BoolToVisibility(bool value) =>
        value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Inverse of <see cref="BoolToVisibility"/>: visible when NOT
    /// <paramref name="value"/>. Drives the canonical row's read-only render — the
    /// mutating controls are present iff the row is not canonical (mail-aliases.md:249);
    /// a collapsed control has no UIA peer, so the e2e per-control n-1 count holds.</summary>
    public static Visibility BoolToVisibilityInverse(bool value) =>
        value ? Visibility.Collapsed : Visibility.Visible;
}
