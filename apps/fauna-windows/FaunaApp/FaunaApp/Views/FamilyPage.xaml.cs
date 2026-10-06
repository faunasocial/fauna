using System;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Views;

/// <summary>
/// The <c>family</c> page (family-safety.md § App surface) — a dumb
/// renderer of the shared <see cref="FamilyViewModel"/>, mirroring
/// <c>AdminUsersPage</c>'s consumption shape: x:Bind for simple leaf
/// properties, imperative code-behind for structural section visibility and
/// the fixed-value-set ComboBoxes (neither maps to a plain x:Bind expression).
/// </summary>
public sealed partial class FamilyPage : Page
{
    private FamilyViewModel? _vm;
    internal FamilyViewModel? ViewModel => _vm;

    // The unknown-sender-mail / feed-sources / content-floor option catalogs are
    // no longer a local literal — PopulateSelect below pulls them live from the
    // shared fauna_core::format catalogs (FaunaFfiMethods.UnknownSenderOptions() /
    // FeedSourcesOptions() / ContentFloorOptions()) at each call site
    // (family-safety.md § Where logic lives).

    private bool _initializingSelect;

    public FamilyPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients && _vm is null)
        {
            _vm = new FamilyViewModel(clients.Rpc!);
            _vm.PropertyChanged += ViewModel_PropertyChanged;
            _vm.Wards.CollectionChanged += (_, _) => UpdateGuardianSectionVisibility();
            _vm.Approvals.CollectionChanged += (_, _) => UpdateNoApprovalsText();
            _vm.IncomingTransfers.CollectionChanged += (_, _) => UpdateIncomingTransfersSectionVisibility();
            _vm.Devices.CollectionChanged += (_, _) => UpdateNoWardDevicesText();
            Bindings.Update();
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        WardsList.ItemsSource = _vm.Wards;
        ApprovalsList.ItemsSource = _vm.Approvals;
        IncomingTransfersList.ItemsSource = _vm.IncomingTransfers;
        DevicesList.ItemsSource = _vm.Devices;
        PopulateSelect(UnknownSenderSelect, FaunaFfiMethods.UnknownSenderOptions(), _vm.PolicyUnknownSenderMail, failClosed: "hold");
        PopulateSelect(FeedSourcesSelect, FaunaFfiMethods.FeedSourcesOptions(), _vm.PolicyFeedSources, failClosed: "block");
        PopulateSelect(UnknownPeerDmSelect, FaunaFfiMethods.UnknownPeerDmOptions(), _vm.PolicyUnknownPeerDm, failClosed: "hold");
        PopulateContentFloorSelects();
        await _vm.LoadCommand.ExecuteAsync(null);
        PopulateSelect(UnknownSenderSelect, FaunaFfiMethods.UnknownSenderOptions(), _vm.PolicyUnknownSenderMail, failClosed: "hold");
        PopulateSelect(FeedSourcesSelect, FaunaFfiMethods.FeedSourcesOptions(), _vm.PolicyFeedSources, failClosed: "block");
        PopulateSelect(UnknownPeerDmSelect, FaunaFfiMethods.UnknownPeerDmOptions(), _vm.PolicyUnknownPeerDm, failClosed: "hold");
        PopulateContentFloorSelects();
        UpdateGuardianSectionVisibility();
        UpdateSupervisedSectionVisibility();
        UpdateNoApprovalsText();
        UpdatePolicyEditorVisibility();
        UpdateIncomingTransfersSectionVisibility();
        UpdateNoWardDevicesText();
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(FamilyViewModel.IsLoading):
                LoadProgress.IsActive = _vm.IsLoading;
                LoadProgress.Visibility = _vm.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(FamilyViewModel.ActionError):
                ErrorText.Text = _vm.ActionError ?? "";
                // Shared `error-message` id — collapse when there's nothing to say
                // (MessageShim.ShouldShow's whitespace-is-absent rule) so the
                // negative `assert not is_visible("error-message")` a fresh page
                // opens with can actually fail (e2e-conventions.md convention 2's
                // rider, obligation (a)).
                var shouldShowError = FaunaApp.Core.Helpers.MessageShim.ShouldShow(_vm.ActionError);
                ErrorText.Visibility = shouldShowError ? Visibility.Visible : Visibility.Collapsed;
                if (shouldShowError)
                {
                    // The page lives in one long ScrollViewer, and ErrorText sits at
                    // the TOP of it — above the wards list, policy editor and
                    // approvals queue a guardian typically scrolls down to reach.
                    // is_visible() checks !IsOffscreen, so a genuinely Visible but
                    // scrolled-away element still reads as invisible
                    // (reference_windows_e2e_is_visible_offscreen). Same deferred
                    // LayoutUpdated dance as IsConfirmingGraduate below: a
                    // Collapsed->Visible flip needs a measure/arrange pass before
                    // StartBringIntoView has a real bounding rect to scroll to.
                    void OnErrorTextLayout(object? s, object le)
                    {
                        if (ErrorText.ActualHeight <= 0) return;
                        ErrorText.StartBringIntoView();
                        ErrorText.LayoutUpdated -= OnErrorTextLayout;
                    }
                    ErrorText.LayoutUpdated += OnErrorTextLayout;
                }
                // Obligation (b): publish into the state-protocol read path so
                // error_text()/has_error() (which try state FIRST) see this page's
                // errors too, mirroring every other page's App.CurrentErrorMessage
                // write (e.g. NotificationsPage.ViewModel_PropertyChanged).
                App.CurrentErrorMessage = _vm.ActionError;
                break;
            case nameof(FamilyViewModel.GuardianHandle):
                UpdateSupervisedSectionVisibility();
                break;
            case nameof(FamilyViewModel.SelectedWardActorId):
                UpdatePolicyEditorVisibility();
                // Unconditional resync of the whole policy editor. Each Policy*
                // property's own case below only fires on a real value CHANGE
                // (the CommunityToolkit.Mvvm [ObservableProperty] equality
                // guard) — so loading, or switching to, a ward whose policy
                // value happens to equal what's already showing (a fresh
                // page's compile-time default, or the previously-selected
                // ward's value) leaves that control stale or never
                // initialized (e.g. FederationToggle's HelpText reads "" —
                // neither "on" nor "off" — instead of the real value).
                // SelectedWardActorId always changes (a fresh byte[] per ward
                // row) and LoadPolicyFromWard sets it last, so this case
                // reliably fires exactly once per ward load/switch with every
                // Policy* property already correct — force EVERY editor control
                // into a known-correct state here rather than trust the
                // individual change notifications. A control added to this page
                // and not added below is a stale-render bug waiting to happen.
                if (_vm.SelectedWardActorId is not null)
                {
                    ContactApprovalToggle.IsOn = _vm.PolicyContactApproval;
                    Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                        ContactApprovalToggle, _vm.PolicyContactApproval ? "on" : "off");
                    FederationToggle.IsOn = _vm.PolicyFederationContact;
                    Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                        FederationToggle, _vm.PolicyFederationContact ? "on" : "off");
                    SelectInCombo(UnknownSenderSelect, _vm.PolicyUnknownSenderMail);
                    SelectInCombo(FeedSourcesSelect, _vm.PolicyFeedSources);
                    // unknown_peer_dm belongs in this unconditional resync for the
                    // same reason: `allow` is both its compile-time default AND the
                    // overwhelmingly common rendered value (an absent wire), so the
                    // per-property case below would rarely fire on a ward switch.
                    SelectInCombo(UnknownPeerDmSelect, _vm.PolicyUnknownPeerDm);
                    // The four content floors + Notify belong in this unconditional
                    // resync for exactly the reason above: `inherit` is every floor's
                    // compile-time default AND the overwhelmingly common stored value,
                    // so the per-property cases below almost never fire — a ward
                    // switch would otherwise leave the previous ward's floor showing.
                    SelectInCombo(ContentNsfwSelect, _vm.PolicyContentNsfw);
                    SelectInCombo(ContentSpamSelect, _vm.PolicyContentSpam);
                    SelectInCombo(ContentPhishingSelect, _vm.PolicyContentPhishing);
                    SelectInCombo(ContentCommercialSelect, _vm.PolicyContentCommercial);
                    ContentNotifyToggle.IsOn = _vm.PolicyContentNotify;
                    Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                        ContentNotifyToggle, _vm.PolicyContentNotify ? "on" : "off");
                    // Screen time: text boxes, so a ward switch between two
                    // wards that happen to share a value (most commonly two
                    // empty/no-limit wards) needs the same unconditional resync
                    // as the combos/toggles above — the per-property cases below
                    // would otherwise never fire.
                    if (ScreenWindowStartBox.Text != _vm.PolicyScreenWindowStart)
                        ScreenWindowStartBox.Text = _vm.PolicyScreenWindowStart;
                    if (ScreenWindowEndBox.Text != _vm.PolicyScreenWindowEnd)
                        ScreenWindowEndBox.Text = _vm.PolicyScreenWindowEnd;
                    if (ScreenDailyMinutesBox.Text != _vm.PolicyScreenDailyMinutes)
                        ScreenDailyMinutesBox.Text = _vm.PolicyScreenDailyMinutes;
                }
                break;
            case nameof(FamilyViewModel.SelectedWardHandle):
                PolicyEditorHeading.Text = _vm.SelectedWardHandle ?? "";
                break;
            case nameof(FamilyViewModel.PolicyContactApproval):
                if (ContactApprovalToggle.IsOn != _vm.PolicyContactApproval)
                    ContactApprovalToggle.IsOn = _vm.PolicyContactApproval;
                // Mirror on/off to HelpText — the uniform cross-app toggle-read
                // idiom (get_attr(id, "state")), same convention as
                // mail-settings-serve-here-toggle.
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                    ContactApprovalToggle, _vm.PolicyContactApproval ? "on" : "off");
                break;
            case nameof(FamilyViewModel.PolicyFederationContact):
                if (FederationToggle.IsOn != _vm.PolicyFederationContact)
                    FederationToggle.IsOn = _vm.PolicyFederationContact;
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                    FederationToggle, _vm.PolicyFederationContact ? "on" : "off");
                break;
            case nameof(FamilyViewModel.PolicyUnknownSenderMail):
                SelectInCombo(UnknownSenderSelect, _vm.PolicyUnknownSenderMail);
                break;
            case nameof(FamilyViewModel.PolicyFeedSources):
                SelectInCombo(FeedSourcesSelect, _vm.PolicyFeedSources);
                break;
            case nameof(FamilyViewModel.PolicyUnknownPeerDm):
                SelectInCombo(UnknownPeerDmSelect, _vm.PolicyUnknownPeerDm);
                break;
            case nameof(FamilyViewModel.PolicyContentNsfw):
                SelectInCombo(ContentNsfwSelect, _vm.PolicyContentNsfw);
                break;
            case nameof(FamilyViewModel.PolicyContentSpam):
                SelectInCombo(ContentSpamSelect, _vm.PolicyContentSpam);
                break;
            case nameof(FamilyViewModel.PolicyContentPhishing):
                SelectInCombo(ContentPhishingSelect, _vm.PolicyContentPhishing);
                break;
            case nameof(FamilyViewModel.PolicyContentCommercial):
                SelectInCombo(ContentCommercialSelect, _vm.PolicyContentCommercial);
                break;
            case nameof(FamilyViewModel.PolicyContentNotify):
                if (ContentNotifyToggle.IsOn != _vm.PolicyContentNotify)
                    ContentNotifyToggle.IsOn = _vm.PolicyContentNotify;
                // Same on/off HelpText mirror as the two toggles above — the
                // cross-app get_attr(id, "state") read idiom.
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                    ContentNotifyToggle, _vm.PolicyContentNotify ? "on" : "off");
                break;
            case nameof(FamilyViewModel.PolicyScreenWindowStart):
                if (ScreenWindowStartBox.Text != _vm.PolicyScreenWindowStart)
                    ScreenWindowStartBox.Text = _vm.PolicyScreenWindowStart;
                break;
            case nameof(FamilyViewModel.PolicyScreenWindowEnd):
                if (ScreenWindowEndBox.Text != _vm.PolicyScreenWindowEnd)
                    ScreenWindowEndBox.Text = _vm.PolicyScreenWindowEnd;
                break;
            case nameof(FamilyViewModel.PolicyScreenDailyMinutes):
                if (ScreenDailyMinutesBox.Text != _vm.PolicyScreenDailyMinutes)
                    ScreenDailyMinutesBox.Text = _vm.PolicyScreenDailyMinutes;
                break;
            case nameof(FamilyViewModel.ContactAddInput):
                if (ContactAddInputBox.Text != _vm.ContactAddInput) ContactAddInputBox.Text = _vm.ContactAddInput;
                break;
            case nameof(FamilyViewModel.TransferInput):
                if (TransferInputBox.Text != _vm.TransferInput) TransferInputBox.Text = _vm.TransferInput;
                break;
            case nameof(FamilyViewModel.IsConfirmingGraduate):
                // Bring the just-revealed confirm into view — a control below the
                // fold fails is_visible (IsOffscreen) even when realized
                // (reference_windows_e2e_is_visible_offscreen). Calling
                // StartBringIntoView() synchronously HERE is a no-op: the button's
                // Visibility flip (x:Bind, a separate PropertyChanged subscriber)
                // hasn't gone through a measure/arrange pass yet, so it still
                // reports its pre-reveal (collapsed, zero-height) bounding rect.
                // Defer with a one-shot LayoutUpdated (mirrors FoldersPage's
                // identical Expander-reveal idiom) — fires repeatedly (cheap no-op)
                // until the button has actually measured, then scrolls once and
                // unsubscribes; no fixed delay, no race window.
                if (_vm.IsConfirmingGraduate)
                {
                    void OnGraduateConfirmLayout(object? s, object le)
                    {
                        if (GraduateConfirmButton.ActualHeight <= 0) return;
                        GraduateConfirmButton.StartBringIntoView();
                        GraduateConfirmButton.LayoutUpdated -= OnGraduateConfirmLayout;
                    }
                    GraduateConfirmButton.LayoutUpdated += OnGraduateConfirmLayout;
                }
                break;
        }
    }

    private void ContactAddInputBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is not null) _vm.ContactAddInput = ContactAddInputBox.Text;
    }

    private void ScreenWindowStartBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is not null) _vm.PolicyScreenWindowStart = ScreenWindowStartBox.Text;
    }

    private void ScreenWindowEndBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is not null) _vm.PolicyScreenWindowEnd = ScreenWindowEndBox.Text;
    }

    private void ScreenDailyMinutesBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is not null) _vm.PolicyScreenDailyMinutes = ScreenDailyMinutesBox.Text;
    }

    private void TransferInputBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is not null) _vm.TransferInput = TransferInputBox.Text;
    }

    private async void AcceptIncomingTransfer_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: FamilyIncomingTransferRow row }) return;
        await _vm.DecideIncomingTransferAsync(row, accept: true);
    }

    private async void DeclineIncomingTransfer_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: FamilyIncomingTransferRow row }) return;
        await _vm.DecideIncomingTransferAsync(row, accept: false);
    }

    private void ContactApprovalToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_vm is not null) _vm.PolicyContactApproval = ContactApprovalToggle.IsOn;
    }

    private void FederationToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_vm is not null) _vm.PolicyFederationContact = FederationToggle.IsOn;
    }

    private void ContentNotifyToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.PolicyContentNotify = ContentNotifyToggle.IsOn;
        // Mirror on/off into HelpText here too, not only in the VM's
        // PropertyChanged case: a user-driven toggle whose new value already
        // equals the VM's raises no PropertyChanged at all ([ObservableProperty]'s
        // equality guard), and without this the state read would go stale.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            ContentNotifyToggle, ContentNotifyToggle.IsOn ? "on" : "off");
    }

    /// <summary>x:Bind Mode=OneWay's initial materialization sets IsOn to the
    /// row's already-current GuardianMarked and still raises Toggled — the
    /// guard below (toggle.IsOn == row.GuardianMarked, i.e. nothing actually
    /// changed) skips that fire and every re-push after a successful RPC
    /// (DeviceMarkAsync updates row.GuardianMarked, which re-fires this same
    /// binding). Only a genuine user flip reaches the RPC. row carries the
    /// device id BY VALUE, so a concurrent refetch reordering the ward's
    /// devices can't route this flip at the wrong one (family-safety.md §
    /// Full visibility for young children, Slice F). On failure, revert the
    /// toggle's visual state to the last-known-good value — x:Bind OneWay
    /// doesn't auto-revert.</summary>
    private async void DeviceMarkToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not ToggleSwitch { DataContext: FamilyDeviceRow row } toggle) return;
        if (toggle.IsOn == row.GuardianMarked) return;
        var ok = await _vm.DeviceMarkAsync(row, toggle.IsOn);
        if (!ok) toggle.IsOn = row.GuardianMarked;
    }

    private void UpdateGuardianSectionVisibility()
    {
        if (_vm is null) return;
        GuardianSection.Visibility = _vm.Wards.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateSupervisedSectionVisibility()
    {
        if (_vm is null) return;
        SupervisedSection.Visibility = _vm.GuardianHandle is not null ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdatePolicyEditorVisibility()
    {
        if (_vm is null) return;
        PolicyEditor.Visibility = _vm.SelectedWardActorId is not null ? Visibility.Visible : Visibility.Collapsed;
        PolicyEditorHeading.Text = _vm.SelectedWardHandle ?? "";
    }

    private void UpdateNoApprovalsText()
    {
        if (_vm is null) return;
        NoApprovalsText.Visibility = _vm.Approvals.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateNoWardDevicesText()
    {
        if (_vm is null) return;
        NoWardDevicesText.Visibility = _vm.Devices.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateIncomingTransfersSectionVisibility()
    {
        if (_vm is null) return;
        IncomingTransfersSection.Visibility = _vm.IncomingTransfers.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Fixed-value-set ComboBoxes (allow/hold/reject, allow/block) ─────────
    // Same programmatic-suppression shape as AdminUsersPage's tier combo.

    private void PopulateSelect(ComboBox combo, ReachPolicyOption[] options, string selected, string failClosed)
    {
        _initializingSelect = true;
        try
        {
            combo.Items.Clear();
            foreach (var opt in options)
            {
                var item = new ComboBoxItem { Content = Strings.Resolve(opt.label), Tag = opt.value };
                combo.Items.Add(item);
                if (opt.value == selected) combo.SelectedItem = item;
            }
            // An unmatched value renders as the strictest option, never the
            // permissive one (family-safety.md § "A knob value a client cannot
            // parse renders fail-closed" — the VM normalizes on load, so this
            // is defense in depth).
            if (combo.SelectedItem is null)
                foreach (var obj in combo.Items)
                    if (obj is ComboBoxItem { Tag: string t } item && t == failClosed)
                    {
                        combo.SelectedItem = item;
                        break;
                    }
        }
        finally
        {
            _initializingSelect = false;
        }
    }

    private void SelectInCombo(ComboBox combo, string value)
    {
        _initializingSelect = true;
        try
        {
            foreach (var obj in combo.Items)
                if (obj is ComboBoxItem { Tag: string t } item && t == value)
                {
                    combo.SelectedItem = item;
                    break;
                }
        }
        finally
        {
            _initializingSelect = false;
        }
    }

    private string SelectedValue(ComboBox combo, string fallback)
        => combo.SelectedItem is ComboBoxItem { Tag: string t } ? t : fallback;

    private void UnknownSenderSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingSelect || _vm is null) return;
        _vm.PolicyUnknownSenderMail = SelectedValue(UnknownSenderSelect, "hold");
    }

    private void FeedSourcesSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingSelect || _vm is null) return;
        _vm.PolicyFeedSources = SelectedValue(FeedSourcesSelect, "block");
    }

    /// <summary>Unlike every sibling reach-knob handler above, this one also arms
    /// <c>PolicyUnknownPeerDmEdited</c> — the guard <see cref="_initializingSelect"/>
    /// already suppresses is exactly what makes "only a genuine user edit reaches
    /// here" true (a programmatic <see cref="PopulateSelect"/>/<see cref="SelectInCombo"/>
    /// call always sets it first), so no separate mechanism is needed
    /// (family-safety.md § The bridge-DM gate → App affordance).</summary>
    private void UnknownPeerDmSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingSelect || _vm is null) return;
        _vm.PolicyUnknownPeerDm = SelectedValue(UnknownPeerDmSelect, "hold");
        _vm.PolicyUnknownPeerDmEdited = true;
    }

    // ── Content-floor selects (inherit/collapse/block) ──────────────────────
    // All four render the ONE shared ContentFloorOptions() catalog, so their value
    // list cannot drift from what fauna.family.policy.update accepts
    // (family-safety.md § Content policy). failClosed: "block" — never `inherit`.

    private void PopulateContentFloorSelects()
    {
        if (_vm is null) return;
        var options = FaunaFfiMethods.ContentFloorOptions();
        PopulateSelect(ContentNsfwSelect, options, _vm.PolicyContentNsfw, failClosed: "block");
        PopulateSelect(ContentSpamSelect, options, _vm.PolicyContentSpam, failClosed: "block");
        PopulateSelect(ContentPhishingSelect, options, _vm.PolicyContentPhishing, failClosed: "block");
        PopulateSelect(ContentCommercialSelect, options, _vm.PolicyContentCommercial, failClosed: "block");
    }

    private void ContentNsfwSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingSelect || _vm is null) return;
        _vm.PolicyContentNsfw = SelectedValue(ContentNsfwSelect, "block");
    }

    private void ContentSpamSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingSelect || _vm is null) return;
        _vm.PolicyContentSpam = SelectedValue(ContentSpamSelect, "block");
    }

    private void ContentPhishingSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingSelect || _vm is null) return;
        _vm.PolicyContentPhishing = SelectedValue(ContentPhishingSelect, "block");
    }

    private void ContentCommercialSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingSelect || _vm is null) return;
        _vm.PolicyContentCommercial = SelectedValue(ContentCommercialSelect, "block");
    }

    // ── Row interactions ─────────────────────────────────────────────────

    private void WardItem_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: FamilyWardRow row }) return;
        _vm.SelectWard(row);
    }

    private async void SavePolicy_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.SavePolicyAsync();
    }

    private async void ApproveApproval_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: FamilyApprovalRow row }) return;
        await _vm.DecideApprovalAsync(row, approve: true);
    }

    private async void DenyApproval_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: FamilyApprovalRow row }) return;
        await _vm.DecideApprovalAsync(row, approve: false);
    }

    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;

    public static Visibility InverseBoolToVisibility(bool value)
        => value ? Visibility.Collapsed : Visibility.Visible;
}
