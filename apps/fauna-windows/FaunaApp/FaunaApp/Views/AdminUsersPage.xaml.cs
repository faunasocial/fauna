using System;
using System.Collections.Generic;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Consolidated admin-users hub — five sections (admin.md § Users): Pending
/// requests / Registration / Admit / Invite / Users, all framed around assigning a
/// TIER. A dumb renderer of the shared <see cref="AdminUsersViewModel"/>
/// (FaunaApp.Core, over the <see cref="INestRpcClient"/> WS-RPC seam →
/// <c>FfiAdminClient</c>; no <c>/admin/api/*</c> twins) — mirrors <c>EventsPage</c> /
/// <c>FeedPage</c> VM consumption. Tier pickers are native <c>ComboBox</c>es (the
/// converged cross-app select shape — web <c>&lt;select&gt;</c>, linux
/// <c>gtk::DropDown</c>), populated from <see cref="AdminUsersViewModel.Tiers"/>.
/// Per-row approve-tier + deny-reason are view-state held here (the VM rows are
/// immutable); the VM owns all RPC + the round-trip refetch. Eviction + pagination
/// are deferred (matches the VM scope + the linux page-1/page-2 split).
/// </summary>
public sealed partial class AdminUsersPage : Page
{
    private AdminUsersViewModel? _vm;

    // Per-row UI state (the Core VM rows are immutable display records):
    // the tier a pending request will be approved at, and its deny reason.
    private readonly Dictionary<long, string> _approveTier = new();
    private readonly Dictionary<long, string> _denyReason = new();
    // The guardian a pending request will be admitted under (family-safety.md
    // § App surface); null = an ordinary account.
    private readonly Dictionary<long, byte[]?> _approveGuardian = new();
    // The band a pending request will be admitted at, as an age_band_options() VALUE
    // (family-safety.md § App surface → Age-band surfaces); absent = the row's
    // AgeBandSeed (the applicant's claim, else not-set). The row's band ComboBox is
    // kept too, so a guardian change on the same row can reset + gate it.
    private readonly Dictionary<long, string> _approveAgeBand = new();
    private readonly Dictionary<long, ComboBox> _requestAgeBandCombos = new();

    public AdminUsersPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients && _vm is null)
        {
            _vm = new AdminUsersViewModel(clients.Rpc!);
            _vm.PropertyChanged += ViewModel_PropertyChanged;
            _vm.Users.CollectionChanged += (_, _) => UpdateUserCount();
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        RequestsList.ItemsSource = _vm.InviteRequests;
        InviteCodesList.ItemsSource = _vm.InviteCodes;
        UsersList.ItemsSource = _vm.Users;
        await _vm.LoadCommand.ExecuteAsync(null);
        PopulateTierCombo(CreateFormTierSelect, _vm.NewCodeTier);
        PopulateGuardianCombo(CreateFormGuardianSelect, _vm.NewCodeGuardianActor);
        PopulateAgeBandCombo(CreateFormAgeBandSelect, _vm.NewCodeAgeBand);
        CreateFormAgeBandSelect.IsEnabled = _vm.NewCodeAgeBandEnabled;
        UpdateRegistrationDisplay();
        SyncAgeVerificationToggle();
        PopulateTierCombo(AdmitTierSelect, string.IsNullOrEmpty(_vm.AdmitTierDraft) ? DefaultTier() : _vm.AdmitTierDraft);
        _vm.AdmitTierDraft = SelectedTier(AdmitTierSelect);
        UpdateUserCount();
        UpdatePagination();
        UpdateRequestsEmpty();
        UpdateCodesEmpty();
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(AdminUsersViewModel.IsLoading):
                LoadProgress.IsActive = _vm.IsLoading;
                LoadProgress.Visibility = _vm.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminUsersViewModel.ActionError):
                // Hub action errors (e.g. fauna.admin.conflict on a stale
                // invite-request approve) route ONLY to the dedicated, hub-local
                // `admin-users-action-error` element — NOT the app-wide
                // `error-message` / state-protocol banner (App.CurrentErrorMessage).
                // admin.md § Users: action errors are hub-scoped. Matches web,
                // which keeps `actionError` a local component variable.
                ActionError.Text = _vm.ActionError ?? "";
                ActionError.Visibility = string.IsNullOrEmpty(_vm.ActionError)
                    ? Visibility.Collapsed : Visibility.Visible;
                break;
            case nameof(AdminUsersViewModel.IsCreatingCode):
                InviteCreateForm.Visibility = _vm.IsCreatingCode ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminUsersViewModel.NewCodeTier):
                SelectTierInCombo(CreateFormTierSelect, _vm.NewCodeTier);
                break;
            case nameof(AdminUsersViewModel.NewCodeGuardianActor):
                SelectGuardianInCombo(CreateFormGuardianSelect, _vm.NewCodeGuardianActor);
                break;
            case nameof(AdminUsersViewModel.NewCodeAgeBand):
                SelectAgeBandInCombo(CreateFormAgeBandSelect, _vm.NewCodeAgeBand);
                break;
            case nameof(AdminUsersViewModel.NewCodeAgeBandEnabled):
                CreateFormAgeBandSelect.IsEnabled = _vm.NewCodeAgeBandEnabled;
                break;
            case nameof(AdminUsersViewModel.AgeVerificationDraft):
                SyncAgeVerificationToggle();
                break;
            case nameof(AdminUsersViewModel.ShowMintedCode):
                CopyCodeButton.Visibility = _vm.ShowMintedCode ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminUsersViewModel.RegistrationMode):
            case nameof(AdminUsersViewModel.RegistrationModeDraft):
                UpdateRegistrationDisplay();
                break;
            case nameof(AdminUsersViewModel.MaxFreeUsersInput):
                if (MaxFreeUsersTextBox.Text != _vm.MaxFreeUsersInput) MaxFreeUsersTextBox.Text = _vm.MaxFreeUsersInput;
                break;
        }
    }

    private void UpdateUserCount()
    {
        if (_vm is null) return;
        UserCountText.Text = Strings.Format("admin/users_page/total", _vm.UserCount);
    }

    /// <summary>Reflect the VM's pagination state — "Page X of Y" plus the prev/next
    /// buttons' enabled state (admin.md § Users pagination). The app DISABLES each
    /// button at its bound — the guard a user meets — so the cross-app test asserts
    /// that directly rather than clicking through it (test_admin_users_hub.py,
    /// 2026-08-28); a disabled WinUI button can't be invoked, so this only works
    /// because nothing clicks at the bound anymore. The VM command handlers
    /// (PrevPageAsync/NextPageAsync) keep their own no-op guard regardless.</summary>
    private void UpdatePagination()
    {
        if (_vm is null) return;
        PaginationIndicator.Text = Strings.Format(
            "admin/users_page/page_indicator", _vm.CurrentPage, _vm.TotalPages);
        PrevPageButton.IsEnabled = _vm.HasPrevPage;
        NextPageButton.IsEnabled = _vm.HasNextPage;
    }

    /// <summary>x:Bind helper for the mutually-exclusive evict/cancel row controls
    /// (mirrors FeedPage.BoolToVisibility — a single call is x:Bind-legal).</summary>
    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;

    // ── Tier ComboBoxes (the converged native select shape; ui.yaml types the three
    //    tier pickers `select`) ────────────────────────────────────────────────
    //
    // WinUI raises SelectionChanged SYNCHRONOUSLY when SelectedItem is set in code, so
    // populating a picker would otherwise fire its change handler — and for the live
    // Users picker that means ChangeUserTierAsync → refetch → row rebuild → Loaded →
    // populate again → an infinite loop. This flag suppresses the handlers while we
    // initialise a picker programmatically (populate is synchronous on the UI thread).
    private bool _initializingTierCombo;

    /// <summary>Fill a tier ComboBox from <see cref="AdminUsersViewModel.Tiers"/> and
    /// select <paramref name="selectedTier"/> — each item's Content is the tier name (so
    /// the e2e <c>driver.select</c> matches the popup item, and <c>get_text</c> reads the
    /// selection back). Programmatic, so the SelectionChanged handlers are suppressed.</summary>
    private void PopulateTierCombo(ComboBox combo, string selectedTier)
    {
        if (_vm is null) return;
        _initializingTierCombo = true;
        try
        {
            combo.Items.Clear();
            foreach (var tier in _vm.Tiers)
            {
                var item = new ComboBoxItem { Content = tier, Tag = tier };
                combo.Items.Add(item);
                if (tier == selectedTier) combo.SelectedItem = item;
            }
            if (combo.SelectedItem is null && combo.Items.Count > 0) combo.SelectedIndex = 0;
        }
        finally
        {
            _initializingTierCombo = false;
        }
    }

    /// <summary>Move a tier ComboBox's selection to <paramref name="tier"/> without
    /// rebuilding its items (reflecting a VM-side change). Programmatic → suppressed.</summary>
    private void SelectTierInCombo(ComboBox combo, string tier)
    {
        _initializingTierCombo = true;
        try
        {
            foreach (var obj in combo.Items)
                if (obj is ComboBoxItem { Tag: string t } item && t == tier)
                {
                    combo.SelectedItem = item;
                    break;
                }
        }
        finally
        {
            _initializingTierCombo = false;
        }
    }

    /// <summary>The tier currently selected in <paramref name="combo"/>, or the first
    /// defined tier if none is selected.</summary>
    private string SelectedTier(ComboBox combo)
        => combo.SelectedItem is ComboBoxItem { Tag: string t } ? t : DefaultTier();

    // Same programmatic-suppression pattern as _initializingTierCombo, kept separate
    // so a guardian-combo populate can't suppress a co-located tier SelectionChanged.
    private bool _initializingGuardianCombo;

    /// <summary>Fill a guardian ComboBox from <see cref="AdminUsersViewModel.GuardianCandidates"/>
    /// — every account on the nest, not just the paginated Users page it sits beside
    /// (admin.md § 2 → <i>Which accounts a picker offers</i>; family-safety.md § App
    /// surface — admission surfaces): a leading "None" item (<c>Tag=null</c>) plus one
    /// item per non-suspended account (<c>Tag</c> = actor id bytes; the nest
    /// re-validates existence/suspension/non-supervision on submit — this filter is UX
    /// only). Selects <paramref name="selectedGuardian"/> by actor id, or "None" if not
    /// found/null.</summary>
    private void PopulateGuardianCombo(ComboBox combo, byte[]? selectedGuardian)
    {
        if (_vm is null) return;
        _initializingGuardianCombo = true;
        try
        {
            combo.Items.Clear();
            var none = new ComboBoxItem { Content = S.Get("admin/users_page/guardian_none"), Tag = null };
            combo.Items.Add(none);
            combo.SelectedItem = none;
            foreach (var user in _vm.GuardianCandidates)
            {
                if (user.Suspended) continue;
                var item = new ComboBoxItem { Content = user.PickerOption, Tag = user.ActorId };
                combo.Items.Add(item);
                if (selectedGuardian is not null && user.ActorId.AsSpan().SequenceEqual(selectedGuardian))
                    combo.SelectedItem = item;
            }
        }
        finally
        {
            _initializingGuardianCombo = false;
        }
    }

    /// <summary>The actor id currently selected in a guardian <paramref name="combo"/>,
    /// or <c>null</c> for "None"/no selection.</summary>
    private byte[]? SelectedGuardian(ComboBox combo)
        => combo.SelectedItem is ComboBoxItem { Tag: byte[] g } ? g : null;

    /// <summary>Move a guardian ComboBox's selection to <paramref name="guardian"/>
    /// (or "None" if not found/null) without rebuilding its items (reflecting a
    /// VM-side change, e.g. <c>BeginCreateCode</c> resetting the mint form).
    /// Programmatic → suppressed.</summary>
    private void SelectGuardianInCombo(ComboBox combo, byte[]? guardian)
    {
        _initializingGuardianCombo = true;
        try
        {
            foreach (var obj in combo.Items)
            {
                if (guardian is not null && obj is ComboBoxItem { Tag: byte[] g } item
                    && g.AsSpan().SequenceEqual(guardian))
                {
                    combo.SelectedItem = item;
                    return;
                }
            }
            if (combo.Items.Count > 0) combo.SelectedIndex = 0; // "None" is always item 0
        }
        finally
        {
            _initializingGuardianCombo = false;
        }
    }

    // ── Age-band ComboBoxes (family-safety.md § App surface → Age-band surfaces) —
    //    the mint form's and each pending-request row's. Own suppression flag, same
    //    rationale as _initializingGuardianCombo. ─────────────────────────────────
    private bool _initializingAgeBandCombo;

    /// <summary>Fill an age-band ComboBox from the shared <c>age_band_options()</c>
    /// catalog (not-set + the four bands, in the ratified order) and select
    /// <paramref name="selectedValue"/> — Content is the localized label while
    /// AutomationProperties.Name is the option VALUE, so FlaUI's exact-Name
    /// <c>Select()</c>/<c>get_text</c> speak what <c>driver.select(id, value)</c> sends
    /// (the registration-mode picker's shape). No per-app spelling of the vocabulary.</summary>
    private void PopulateAgeBandCombo(ComboBox combo, string selectedValue)
    {
        _initializingAgeBandCombo = true;
        try
        {
            combo.Items.Clear();
            foreach (var option in uniffi.fauna_ffi.FaunaFfiMethods.AgeBandOptions())
            {
                var item = new ComboBoxItem { Content = S.Resolve(option.@label), Tag = option.@value };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, option.@value);
                combo.Items.Add(item);
                if (option.@value == selectedValue) combo.SelectedItem = item;
            }
            if (combo.SelectedItem is null && combo.Items.Count > 0) combo.SelectedIndex = 0;
        }
        finally
        {
            _initializingAgeBandCombo = false;
        }
    }

    /// <summary>Move an age-band ComboBox to <paramref name="value"/> without rebuilding
    /// its items (a VM- or guardian-driven reset). Programmatic → suppressed.</summary>
    private void SelectAgeBandInCombo(ComboBox combo, string value)
    {
        _initializingAgeBandCombo = true;
        try
        {
            foreach (var obj in combo.Items)
                if (obj is ComboBoxItem { Tag: string v } item && v == value)
                {
                    combo.SelectedItem = item;
                    break;
                }
        }
        finally
        {
            _initializingAgeBandCombo = false;
        }
    }

    /// <summary>The option VALUE selected in an age-band <paramref name="combo"/>, or
    /// the shared not-set value when nothing is.</summary>
    private static string SelectedAgeBand(ComboBox combo)
        => combo.SelectedItem is ComboBoxItem { Tag: string v } ? v : uniffi.fauna_ffi.FaunaFfiMethods.AgeBandNotSetValue();

    private void CreateFormAgeBandSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingAgeBandCombo || _vm is null) return;
        if (sender is ComboBox combo) _vm.NewCodeAgeBand = SelectedAgeBand(combo);
    }

    // ── The age require-knob's toggle (Registration section) ──────────────────
    private bool _syncingAgeVerificationToggle;

    /// <summary>Paint the toggle from <see cref="AdminUsersViewModel.AgeVerificationDraft"/>
    /// and mirror it onto HelpText ("on"/"off") — the driver's <c>state</c> read, the
    /// family page's toggle idiom. Programmatic → the Toggled handler is suppressed.</summary>
    private void SyncAgeVerificationToggle()
    {
        if (_vm is null) return;
        _syncingAgeVerificationToggle = true;
        try
        {
            if (AgeVerificationToggle.IsOn != _vm.AgeVerificationDraft)
                AgeVerificationToggle.IsOn = _vm.AgeVerificationDraft;
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
                AgeVerificationToggle, _vm.AgeVerificationDraft ? "on" : "off");
        }
        finally
        {
            _syncingAgeVerificationToggle = false;
        }
    }

    private void AgeVerificationToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncingAgeVerificationToggle || _vm is null) return;
        _vm.AgeVerificationDraft = AgeVerificationToggle.IsOn;
    }

    private void UpdateRequestsEmpty()
    {
        if (_vm is null) return;
        NoRequestsText.Visibility = _vm.InviteRequests.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateCodesEmpty()
    {
        if (_vm is null) return;
        NoCodesText.Visibility = _vm.InviteCodes.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Registration ComboBox — a separate suppression flag from the tier /
    //    guardian combos (same rationale as _initializingGuardianCombo: keeps
    //    an unrelated populate from suppressing this section's own
    //    SelectionChanged) ───────────────────────────────────────────────
    private bool _initializingRegistrationCombo;

    /// <summary>Fill the registration-mode ComboBox from the shared
    /// <c>registration_mode_options()</c> catalog (wire value + localized label) and
    /// select <paramref name="selectedWireValue"/> — Content is the LOCALIZED label
    /// (display) while AutomationProperties.Name is the raw wire value: the two
    /// must differ so FlaUI's exact-Name <c>Select()</c>/<c>get_text</c> can target
    /// what <c>driver.select(id, value)</c> drives and dispatches — mirrors
    /// <c>TaskDelegationRowVm.OptionKey</c>'s picker (SettingsTaskDelegationPage) /
    /// <c>EmailFilterPanel</c>'s static items, since the tier pickers' Content==wire
    /// shortcut doesn't hold here (the mode's label is genuinely localized text).</summary>
    private void PopulateRegistrationModeCombo(ComboBox combo, string selectedWireValue)
    {
        _initializingRegistrationCombo = true;
        try
        {
            combo.Items.Clear();
            foreach (var option in uniffi.fauna_ffi.FaunaFfiMethods.RegistrationModeOptions())
            {
                var item = new ComboBoxItem { Content = S.Resolve(option.@label), Tag = option.@value };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, option.@value);
                combo.Items.Add(item);
                if (option.@value == selectedWireValue) combo.SelectedItem = item;
            }
            if (combo.SelectedItem is null && combo.Items.Count > 0) combo.SelectedIndex = 0;
        }
        finally
        {
            _initializingRegistrationCombo = false;
        }
    }

    /// <summary>The wire value currently selected in the registration-mode
    /// <paramref name="combo"/>, or "" if none.</summary>
    private static string SelectedRegistrationMode(ComboBox combo)
        => combo.SelectedItem is ComboBoxItem { Tag: string t } ? t : "";

    /// <summary>Move the registration-mode <paramref name="combo"/>'s selection to
    /// <paramref name="wireValue"/> WITHOUT rebuilding its items (reflecting a
    /// VM-side change) — mirrors <see cref="SelectTierInCombo"/>. Required (not
    /// just cheaper) on the live-selection path: <see cref="RegistrationModeSelect_SelectionChanged"/>
    /// sets <c>RegistrationModeDraft</c> from INSIDE FlaUI's own
    /// <c>SelectionItem.Select()</c> call on a popup <c>ListItem</c>, so
    /// <see cref="UpdateRegistrationDisplay"/> re-enters synchronously on the SAME
    /// UI-thread callback; <c>Items.Clear()</c>-and-rebuild there tears down the
    /// very item FlaUI is mid-selecting and reliably crashed the app process
    /// (measured — 500 "process is not running" from the FlaUI bridge on the
    /// very first live re-select in this section's e2e coverage).</summary>
    private void SelectRegistrationModeInCombo(ComboBox combo, string wireValue)
    {
        _initializingRegistrationCombo = true;
        try
        {
            foreach (var obj in combo.Items)
                if (obj is ComboBoxItem { Tag: string t } item && t == wireValue)
                {
                    combo.SelectedItem = item;
                    break;
                }
        }
        finally
        {
            _initializingRegistrationCombo = false;
        }
    }

    /// <summary>Paint the Registration section from the VM's persisted posture
    /// (admin.md § 2 Users → Section 2) — the read-only explainer when the posture
    /// is unknown/absent (never a picker+save, which could overwrite the nest's
    /// real posture with a guess — public-mode.md § Implementation status today),
    /// else the picker+input+save. Mirrors tui's <c>registration_section</c> /
    /// AdminVM.swift's <c>registrationSection</c>. Full item rebuild only when the
    /// panel is newly becoming visible; an already-visible panel only moves its
    /// selection (<see cref="SelectRegistrationModeInCombo"/> — see its doc for why
    /// a rebuild there is unsafe, not just wasteful).</summary>
    private void UpdateRegistrationDisplay()
    {
        if (_vm is null) return;
        var wasKnown = RegistrationKnownPanel.Visibility == Visibility.Visible;
        var known = !string.IsNullOrEmpty(_vm.RegistrationModeDraft);
        RegistrationUnknownText.Visibility = known ? Visibility.Collapsed : Visibility.Visible;
        RegistrationKnownPanel.Visibility = known ? Visibility.Visible : Visibility.Collapsed;
        if (known)
        {
            if (wasKnown)
                SelectRegistrationModeInCombo(RegistrationModeSelect, _vm.RegistrationModeDraft);
            else
                PopulateRegistrationModeCombo(RegistrationModeSelect, _vm.RegistrationModeDraft);
        }
        else
        {
            RegistrationUnknownText.Text = Strings.Format(
                "admin/users_page/registration_mode_unknown", _vm.RegistrationMode ?? "");
        }
    }

    private void RegistrationModeSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingRegistrationCombo || _vm is null) return;
        if (sender is ComboBox combo) _vm.RegistrationModeDraft = SelectedRegistrationMode(combo);
    }

    private void MaxFreeUsersTextBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is null) return;
        _vm.MaxFreeUsersInput = MaxFreeUsersTextBox.Text ?? "";
    }

    private async void SaveRegistration_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.SaveRegistrationCommand.ExecuteAsync(null);
    }

    // ── Admit ────────────────────────────────────────────────────────────

    private void AdmitActorTextBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is null) return;
        _vm.AdmitActorInput = AdmitActorTextBox.Text ?? "";
    }

    private void AdmitHandleTextBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is null) return;
        _vm.AdmitHandleInput = AdmitHandleTextBox.Text ?? "";
    }

    private void AdmitTierSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingTierCombo || _vm is null) return;
        if (sender is ComboBox combo) _vm.AdmitTierDraft = SelectedTier(combo);
    }

    private async void AdmitUser_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.AdmitUserCommand.ExecuteAsync(null);
    }

    // ── Section 1: Pending requests ────────────────────────────────────────

    /// <summary>Populate a request's approve-tier ComboBox and default the selection to
    /// the row's pending choice (first defined tier on first render).</summary>
    private void RequestTierSelect_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not ComboBox { DataContext: AdminInviteRequestRow row } combo) return;
        var tier = _approveTier.TryGetValue(row.Id, out var t) ? t : DefaultTier();
        PopulateTierCombo(combo, tier);
        _approveTier[row.Id] = SelectedTier(combo);
    }

    private void RequestTierSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingTierCombo) return;
        if (sender is not ComboBox { DataContext: AdminInviteRequestRow row } combo) return;
        _approveTier[row.Id] = SelectedTier(combo);
    }

    /// <summary>Populate a request's approve-guardian ComboBox and default the
    /// selection to the row's pending choice ("None" on first render).</summary>
    private void RequestGuardianSelect_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not ComboBox { DataContext: AdminInviteRequestRow row } combo) return;
        var guardian = _approveGuardian.TryGetValue(row.Id, out var g) ? g : null;
        PopulateGuardianCombo(combo, guardian);
        _approveGuardian[row.Id] = SelectedGuardian(combo);
    }

    private void RequestGuardianSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingGuardianCombo) return;
        if (sender is not ComboBox { DataContext: AdminInviteRequestRow row } combo) return;
        var guardian = SelectedGuardian(combo);
        _approveGuardian[row.Id] = guardian;
        // A band presupposes a guardian: clearing one clears the other, and the row's
        // band select is live only while a guardian is picked.
        if (guardian is null) _approveAgeBand[row.Id] = uniffi.fauna_ffi.FaunaFfiMethods.AgeBandNotSetValue();
        if (_requestAgeBandCombos.TryGetValue(row.Id, out var bandCombo))
        {
            if (guardian is null) SelectAgeBandInCombo(bandCombo, _approveAgeBand[row.Id]);
            bandCombo.IsEnabled = guardian is not null;
        }
    }

    /// <summary>Populate a request's age-band ComboBox — the row's pending pick, else
    /// its <see cref="AdminInviteRequestRow.AgeBandSeed"/> (the applicant's claim, via
    /// the shared <c>claimed_age_band_option</c>) — and gate it on the row's guardian.</summary>
    private void RequestAgeBandSelect_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not ComboBox { DataContext: AdminInviteRequestRow row } combo) return;
        var band = _approveAgeBand.TryGetValue(row.Id, out var b) ? b : row.AgeBandSeed;
        PopulateAgeBandCombo(combo, band);
        _approveAgeBand[row.Id] = SelectedAgeBand(combo);
        _requestAgeBandCombos[row.Id] = combo;
        combo.IsEnabled = _approveGuardian.TryGetValue(row.Id, out var g) && g is not null;
    }

    private void RequestAgeBandSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingAgeBandCombo) return;
        if (sender is not ComboBox { DataContext: AdminInviteRequestRow row } combo) return;
        _approveAgeBand[row.Id] = SelectedAgeBand(combo);
    }

    private void ApproveButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/invite_requests_page/approve");
    }

    private void DenyButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/invite_requests_page/deny");
    }

    private void DenyReason_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (sender is TextBox { DataContext: AdminInviteRequestRow row } tb)
            _denyReason[row.Id] = tb.Text;
    }

    private async void ApproveRequest_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: AdminInviteRequestRow row }) return;
        var tier = _approveTier.TryGetValue(row.Id, out var t) ? t : DefaultTier();
        var guardian = _approveGuardian.TryGetValue(row.Id, out var g) ? g : null;
        var band = _approveAgeBand.TryGetValue(row.Id, out var b) ? b : row.AgeBandSeed;
        await _vm.ApproveRequestAsync(row, tier, guardian, band);
        UpdateRequestsEmpty();
    }

    private async void DenyRequest_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: AdminInviteRequestRow row }) return;
        var reason = _denyReason.TryGetValue(row.Id, out var r) && !string.IsNullOrWhiteSpace(r) ? r : null;
        await _vm.DenyRequestAsync(row, reason);
        UpdateRequestsEmpty();
    }

    // ── Section 2: Invite ──────────────────────────────────────────────────

    private void CreateInviteButton_Click(object sender, RoutedEventArgs e)
        => _vm?.BeginCreateCodeCommand.Execute(null);

    private void CancelCreateButton_Click(object sender, RoutedEventArgs e)
        => _vm?.CancelCreateCodeCommand.Execute(null);

    private void CreateFormTierSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingTierCombo || _vm is null) return;
        if (sender is ComboBox combo) _vm.NewCodeTier = SelectedTier(combo);
    }

    private void CreateFormGuardianSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingGuardianCombo || _vm is null) return;
        if (sender is ComboBox combo) _vm.NewCodeGuardianActor = SelectedGuardian(combo);
    }

    private async void ConfirmCreateButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.NewCodeUses = int.TryParse(MaxUsesInput.Text?.Trim(), out var n) && n >= 1 ? n : 1;
        await _vm.CreateCodeCommand.ExecuteAsync(null);
        UpdateCodesEmpty();
    }

    private void CopyCodeButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm?.MintedCode is not { Length: > 0 } code) return;
        try
        {
            FaunaApp.Helpers.ClipboardHelper.CopyText(code);
        }
        catch
        {
            // Clipboard contention is non-fatal; the code remains visible to copy manually.
        }
    }

    private async void DeleteInviteCode_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: AdminInviteCodeRow row }) return;
        await _vm.DeleteCodeAsync(row.Code);
        UpdateCodesEmpty();
    }

    // ── Section 3: Users ───────────────────────────────────────────────────

    /// <summary>Populate a user row's tier ComboBox and select the row's current tier.
    /// Programmatic → suppressed, so showing the applied tier never re-fires an update.</summary>
    private void UserTierSelect_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not ComboBox { DataContext: AdminUserRow row } combo) return;
        PopulateTierCombo(combo, row.Tier);
    }

    private async void UserTierSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingTierCombo || _vm is null) return;
        if (sender is not ComboBox { DataContext: AdminUserRow row } combo) return;
        var tier = SelectedTier(combo);
        if (tier == row.Tier) return; // no-op selection (defensive)
        await _vm.ChangeUserTierAsync(row, tier);
    }

    private void SuspendButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/users_page/suspend");
    }

    private void EvictButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/users_page/evict");
    }

    private void CancelEvictionButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/users_page/cancel_eviction");
    }

    private async void SuspendUser_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: AdminUserRow row }) return;
        await _vm.SuspendUserAsync(row);
    }

    private async void EvictUser_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: AdminUserRow row }) return;
        await _vm.EvictUserAsync(row);
    }

    private async void CancelEviction_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: AdminUserRow row }) return;
        await _vm.CancelEvictionAsync(row);
    }

    private void MakeAdminButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/users_page/make_admin");
    }

    private void RemoveAdminButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/users_page/remove_admin");
    }

    private async void MakeAdmin_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: AdminUserRow row }) return;
        await _vm.MakeAdminAsync(row);
    }

    private async void RemoveAdmin_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: AdminUserRow row }) return;
        await _vm.RemoveAdminAsync(row);
    }

    private async void PrevPage_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.PrevPageAsync();
        UpdatePagination();
    }

    private async void NextPage_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.NextPageAsync();
        UpdatePagination();
    }

    private string DefaultTier()
        => _vm is { Tiers.Count: > 0 } ? _vm.Tiers[0] : "free";
}
