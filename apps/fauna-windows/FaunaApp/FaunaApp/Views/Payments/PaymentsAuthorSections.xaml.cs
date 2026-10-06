using System.Collections.Generic;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel.DataTransfer;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The author-side payments surface of the profile Tiers tab — §4 payment providers
/// and §5 manual claim codes (monetization.md § Pillar 3), lifted out of
/// <see cref="ProfilePage"/> whole so the whole plane is one removable build item.
/// </summary>
/// <remarks>
/// This is a dumb renderer over <see cref="ProfileViewModel"/>, exactly as the page
/// it came from: the page still owns the view model, hydration and every non-payments
/// section, and drives this control through the three members below. Splitting the
/// view model too would have put payments state behind a second seam for no excision
/// benefit — the view model lives in FaunaApp.Core, whose payments members are
/// removed by the <c>PAYMENTS</c> define rather than by an item.
///
/// ⚠ Nothing may name this type from markup — see the header of the .xaml.
/// </remarks>
public sealed partial class PaymentsAuthorSections : UserControl
{
    private ProfileViewModel? _vm;

    public PaymentsAuthorSections()
    {
        InitializeComponent();
    }

    /// <summary>Bind to the page's view model. Called once, from the host page's
    /// <c>Page_Loaded</c>, before its first <see cref="UpdateDerivedUi"/>.</summary>
    public void Attach(ProfileViewModel vm)
    {
        _vm = vm;
        ProvidersList.ItemsSource = vm.Providers;
        ClaimsList.ItemsSource = vm.Claims;
        // §4's kind select is static (the shared fauna-payments registry) — populate
        // once, unlike the tier pickers which track the mutable TierNames collection.
        PopulateComboBox(ProviderKindSelect, vm.KnownProviderKinds, vm.KnownProviderKinds.FirstOrDefault() ?? "");
    }

    /// <summary>The payments half of the host page's property-changed dispatch.
    /// Returns true when <paramref name="propertyName"/> was one of ours, so the page
    /// can keep its own switch total.</summary>
    public bool OnViewModelPropertyChanged(string propertyName)
    {
        if (_vm is null) return false;
        switch (propertyName)
        {
            case nameof(ProfileViewModel.ShowProviderForm):
                ProviderForm.Visibility = _vm.ShowProviderForm ? Visibility.Visible : Visibility.Collapsed;
                // Bring the just-revealed form into view — a control below the fold
                // fails is_visible (IsOffscreen) even when realized
                // (reference_windows_e2e_is_visible_offscreen). A synchronous call
                // here is a no-op: the Visibility flip hasn't gone through a
                // measure/arrange pass yet, so it still reports the pre-reveal
                // (collapsed, zero-height) bounding rect. Defer with a one-shot
                // LayoutUpdated (mirrors FamilyPage's GraduateConfirmButton /
                // FoldersPage's Expander reveal idiom) — fires repeatedly (cheap
                // no-op) until the form has actually measured, then scrolls once
                // and unsubscribes; no fixed delay, no race window.
                if (_vm.ShowProviderForm)
                {
                    void OnProviderFormLayout(object? s, object le)
                    {
                        if (ProviderForm.ActualHeight <= 0) return;
                        ProviderForm.StartBringIntoView();
                        ProviderForm.LayoutUpdated -= OnProviderFormLayout;
                    }
                    ProviderForm.LayoutUpdated += OnProviderFormLayout;
                }
                return true;
            case nameof(ProfileViewModel.ProviderWebhookUrl):
                ProviderWebhookUrlBox.Text = _vm.ProviderWebhookUrl;
                return true;
            default:
                return false;
        }
    }

    /// <summary>The payments half of the host page's <c>UpdateDerivedUi</c> — placeholder
    /// visibilities and the two tier pickers, whose options track the mutable
    /// <c>TierNames</c> collection.</summary>
    public void UpdateDerivedUi()
    {
        if (_vm is null) return;
        NoProvidersText.Visibility = _vm.Providers.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        NoClaimsText.Visibility = _vm.Claims.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        ProviderForm.Visibility = _vm.ShowProviderForm ? Visibility.Visible : Visibility.Collapsed;
        // TierNames may have grown/shrunk this hydrate (a tier created/deleted) —
        // keep the §4/§5 tier pickers' options in sync, preserving the VM's
        // current selection (the §3 TierSelect pattern, value/Tag split for
        // FlaUI Select() exactness — reference_windows_flaui_select_exact_name).
        PopulateComboBox(ProviderTierSelect, _vm.TierNames, _vm.ProviderFormTier);
        PopulateComboBox(ClaimTierSelect, _vm.TierNames, _vm.ClaimTier);
    }

    // ── §4 Payment providers ────────────────────────────────────────────
    private void AddProvider_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.OpenProviderForm();
        ProviderSecretBox.Password = "";
        PopulateComboBox(ProviderKindSelect, _vm.KnownProviderKinds, _vm.ProviderFormKind);
        PopulateComboBox(ProviderTierSelect, _vm.TierNames, _vm.ProviderFormTier);
    }

    private void ProviderKindSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_vm is null || ProviderKindSelect.SelectedItem is not ComboBoxItem { Tag: string kind }) return;
        _vm.ProviderFormKind = kind;   // triggers UpdateWebhookUrlPreview via the VM's OnProviderFormKindChanged
    }

    private void CancelProviderForm_Click(object sender, RoutedEventArgs e) => _vm?.CancelProviderForm();

    private async void SaveProviderForm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.ProviderFormSecret = ProviderSecretBox.Password;
        if (ProviderTierSelect.SelectedItem is ComboBoxItem { Tag: string tier })
            _vm.ProviderFormTier = tier;
        await _vm.SaveProviderFormAsync();
        ProviderSecretBox.Password = "";
        UpdateDerivedUi();
    }

    private async void RemoveProvider_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: PaymentProviderRow row }) return;
        await _vm.RemoveProviderAsync(row.Kind);
        UpdateDerivedUi();
    }

    /// <summary>Copy the live webhook-URL preview to the clipboard (the
    /// <c>profile-actor-id-copy-btn</c> pattern).</summary>
    private void CopyProviderWebhookUrl_Click(object sender, RoutedEventArgs e)
    {
        var dp = new DataPackage();
        dp.SetText(ProviderWebhookUrlBox.Text);
        Clipboard.SetContent(dp);
    }

    // ── §5 Manual claim codes ───────────────────────────────────────────
    private async void MintClaim_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        if (ClaimTierSelect.SelectedItem is ComboBoxItem { Tag: string tier })
            _vm.ClaimTier = tier;
        await _vm.MintClaimAsync();
        UpdateDerivedUi();
    }

    /// <summary>Rebuild <paramref name="combo"/>'s items from <paramref name="values"/>
    /// (Content = the value; <c>Tag</c> + an explicit <c>AutomationProperties.Name</c>
    /// also carry the raw value, so <c>driver.select(id, value)</c> matches exactly —
    /// reference_windows_flaui_select_exact_name, the folder-conflict-policy-select
    /// precedent). Preserves <paramref name="selected"/>
    /// if still present, else the first item.</summary>
    /// <remarks>Travelled here with its only three call sites (the provider-kind,
    /// provider-tier and claim-tier pickers); ProfilePage's own §3 picker binds
    /// <c>ItemsSource</c> instead.</remarks>
    private static void PopulateComboBox(ComboBox combo, IEnumerable<string> values, string selected)
    {
        combo.Items.Clear();
        foreach (var value in values)
        {
            var item = new ComboBoxItem { Content = value, Tag = value };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, value);
            combo.Items.Add(item);
        }
        var match = combo.Items.OfType<ComboBoxItem>().FirstOrDefault(i => (string)i.Tag == selected);
        combo.SelectedItem = match ?? combo.Items.OfType<ComboBoxItem>().FirstOrDefault();
    }
}
