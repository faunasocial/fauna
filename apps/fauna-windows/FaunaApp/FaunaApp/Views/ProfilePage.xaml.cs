using System;
using System.ComponentModel;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Conversations;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_conversations;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// The Profile page — the canonical per-user SELF detail surface (profile.md),
/// reached via the top-level <c>profile-tab</c>. A dumb renderer of
/// <see cref="ProfileViewModel"/> (FaunaApp.Core) over the <see cref="INestRpcClient"/>
/// seam: the header (handle fallback = actor_id + copy + the inert edit
/// forward-pointer), a tab strip, and the Tiers tab's three subscriptions Slice-A
/// author-management sections (monetization.md § Pillar 1). All projection / WS-RPC
/// sequencing + the encrypted-mode KeyBlob mint+upload live in shared Rust
/// (priority #2). No <c>ConfigureAwait(false)</c> in these handlers (off-thread
/// bound-state mutation throws a silent COMException —
/// reference_windows_vm_configureawait_comexception). Reference renderers: linux
/// apps/fauna-linux/src/views/profile/{mod,tiers}.rs + apple FaunaKit
/// ProfileView/SubscriptionsVM.
/// </summary>
public sealed partial class ProfilePage : Page
{
    /// <summary>The live instance, set on load / cleared on unload — the TestAgent's
    /// <c>compose.file</c> route for <c>profile-edit-avatar</c>/<c>profile-edit-banner</c>
    /// (native bridges can't drive the real OS file picker; mirrors
    /// <c>ConversationsPage.Current</c>/<c>StageAttachment</c>).</summary>
    internal static ProfilePage? Current { get; private set; }

    private ProfileViewModel? _vm;

#if PAYMENTS
    /// <summary>The §§4-5 payments surface, built in code and dropped into
    /// <c>PaymentsSectionsHost</c>. Not named from markup: its type is removed from the
    /// store-safe build (FaunaApp.csproj drops Views\Payments\**), and a XAML element
    /// naming it would fail to compile there. Null until the first
    /// <c>Page_Loaded</c>.</summary>
    private Views.Payments.PaymentsAuthorSections? _payments;

    /// <summary>The gated <c>subscription-tier-form-asking-price</c> input, dropped
    /// into <c>TierAskingPriceHost</c> — same removable-item reasoning as
    /// <see cref="_payments"/> (dynamic-features.md § A gated plane's user-facing
    /// INPUTS excise with it). Null until the first <c>Page_Loaded</c>.</summary>
    private Views.Payments.AskingPriceInput? _tierAskingPriceInput;
#endif

    // The nav-parameter clients (held so the OTHER-profile start-DM resolves the
    // SAME ConversationsManager the Conversations page renders off — the login
    // session's wired manager in production, ConversationsManagerHost.Instance in
    // E2E). Null until OnNavigatedTo.
    private ServiceClients? _clients;

    // Set while a programmatic SelectedItem update syncs the §3 tier picker to the
    // VM, so reflecting state never echoes back as a roster-refresh dispatch.
    private bool _syncingTierSelect;

    public ProfilePage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        // `copied` (the copy button's HelpText) is cleared on every profile open, so a
        // copy on one profile can never read back as the next profile's.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(CopyActorIdButton, "");
        if (e.Parameter is ServiceClients clients && clients.Rpc is not null)
        {
            _clients = clients;
            // OTHER profile when a tap-through / state-protocol nav stashed a target
            // (profile.md § Layout & flow → Another's profile); else the viewer's own
            // (SELF). Read-and-clear so the next SELF nav (sidebar profile-tab) is SELF.
            var target = App.PendingProfileTarget;
            App.PendingProfileTarget = null;
            var isSelf = string.IsNullOrEmpty(target);
            var actorId = isSelf
                ? (clients.Crypto.HasKey ? clients.Crypto.ActorIdHex : "")
                : target!;
            _vm = new ProfileViewModel(clients.Rpc, actorId, isSelf);
            _vm.PropertyChanged += ViewModel_PropertyChanged;

            // Rich identity is publish-path-gated — render the actor_id (the handle
            // fallback, mirroring linux build_header / apple displayName).
            HandleText.Text = actorId;
            PageHeading.Text = S.Get("profile/posts");

            // Header primary action (edit vs follow) + Tiers-tab branch (author
            // management vs offers browse) follow the is_self split.
            ApplyProfileMode(isSelf);
        }
    }

    /// <summary>Toggle the SELF vs OTHER surfaces: the header primary action
    /// (<c>profile-edit-button</c> vs <c>profile-follow-button</c>) and the Tiers-tab
    /// branch (the SELF author-management sections vs the OTHER offers section).</summary>
    private void ApplyProfileMode(bool isSelf)
    {
        EditButton.Visibility = isSelf ? Visibility.Visible : Visibility.Collapsed;
        FollowButton.Visibility = isSelf ? Visibility.Collapsed : Visibility.Visible;
        // Secondary relationship actions (start-DM + block) — OTHER profile only.
        SecondaryActions.Visibility = isSelf ? Visibility.Collapsed : Visibility.Visible;
        SelfAuthorSections.Visibility = isSelf ? Visibility.Visible : Visibility.Collapsed;
        OffersSection.Visibility = isSelf ? Visibility.Collapsed : Visibility.Visible;
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        // Bind the section lists + the §3 tier picker to the VM's observable
        // collections (auto-update on every re-read).
        TiersList.ItemsSource = _vm.Tiers;
        RequestsList.ItemsSource = _vm.Requests;
        SubscribersList.ItemsSource = _vm.Subscribers;
        TierSelect.ItemsSource = _vm.TierNames;
        EditLinksList.ItemsSource = _vm.EditLinks;
        OffersList.ItemsSource = _vm.Offers;   // OTHER profile — offered tiers

#if PAYMENTS
        // §§4-5 live in their own removable build item — bind them the same way,
        // one seam further out (dynamic-features.md § Platform-family surface excision).
        _payments = new Views.Payments.PaymentsAuthorSections();
        PaymentsSectionsHost.Content = _payments;
        _payments.Attach(_vm);

        _tierAskingPriceInput = new Views.Payments.AskingPriceInput
        {
            AutomationId = Ids.SubscriptionTierFormAskingPrice,
            Header = S.Get("subscriptions/asking_price"),
        };
        TierAskingPriceHost.Content = _tierAskingPriceInput;
#endif

        Current = this;

        // Render the published display_name (else the actor-id fallback) into the
        // header on load (the HeaderName change pushes through ViewModel_PropertyChanged).
        await _vm.RefreshHeaderAsync();
        HandleText.Text = _vm.HeaderName;

        await _vm.HydrateAsync();
        UpdateDerivedUi();

        // OTHER profile: read the viewed actor's contact edge so the
        // profile-block-button toggle opens with the right label (Unblock when
        // already blocked, else Block) — mirrors linux refresh_block_state.
        if (!_vm.IsSelf)
        {
            ApplyFollowLabel(false);
            await _vm.RefreshBlockStateAsync();
            ApplyBlockLabel();
        }
    }

    private void Page_Unloaded(object sender, RoutedEventArgs e)
    {
        if (ReferenceEquals(Current, this)) Current = null;
    }

    /// <summary>Render the <c>profile-block-button</c> toggle label from the VM's
    /// <see cref="ProfileViewModel.IsBlocked"/> — "Unblock" when blocked, "Block"
    /// otherwise — off the shared <c>fauna_core::format::contact_toggle_block_label</c>
    /// (FFI <c>ContactToggleBlockLabel</c>; contacts.md § Where logic lives → Unblock),
    /// the single source the five apps that surface the button share. One place
    /// renders it (the initial OTHER-profile read + each successful toggle), mirroring
    /// linux <c>apply_block_state</c>. The button *style* stays per-app.</summary>
    private void ApplyBlockLabel()
    {
        if (_vm is null) return;
        BlockButton.Content = S.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.ContactToggleBlockLabel(_vm.IsBlocked));
    }

    /// <summary>Render the <c>profile-follow-button</c> toggle label — "Following"
    /// when followed, "Follow" otherwise — off the shared
    /// <c>fauna_core::format::follow_toggle_label</c> (FFI <c>FollowToggleLabel</c>;
    /// profile.md § Where logic lives → Follow / unfollow), the single source every
    /// app sharing the button resolves. Mirrors <see cref="ApplyBlockLabel"/>.</summary>
    private void ApplyFollowLabel(bool isFollowing)
    {
        FollowButton.Content = S.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.FollowToggleLabel(isFollowing));
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(ProfileViewModel.ErrorMessage):
                RenderError(_vm.ErrorMessage);
                break;
            case nameof(ProfileViewModel.ShowForm):
                TierForm.Visibility = _vm.ShowForm ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(ProfileViewModel.ShowEditForm):
                ProfileEditForm.Visibility = _vm.ShowEditForm ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(ProfileViewModel.HeaderName):
                HandleText.Text = _vm.HeaderName;
                break;
            case nameof(ProfileViewModel.IsBlocked):
                ApplyBlockLabel();
                break;
            case nameof(ProfileViewModel.IsApproving):
                RequestBusyText.Visibility = _vm.IsApproving ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(ProfileViewModel.SelectedTier):
                SyncTierSelect();
                break;
#if PAYMENTS
            // ShowProviderForm / ProviderWebhookUrl — handled by the payments control,
            // which owns the elements they drive.
            default:
                _payments?.OnViewModelPropertyChanged(e.PropertyName ?? "");
                break;
#endif
        }
    }

    // ── Tab strip ───────────────────────────────────────────────────────
    private void PostsTab_Click(object sender, RoutedEventArgs e)
    {
        PostsPanel.Visibility = Visibility.Visible;
        TiersPanel.Visibility = Visibility.Collapsed;
        PageHeading.Text = S.Get("profile/posts");
    }

    private async void TiersTab_Click(object sender, RoutedEventArgs e)
    {
        PostsPanel.Visibility = Visibility.Collapsed;
        TiersPanel.Visibility = Visibility.Visible;
        PageHeading.Text = S.Get("profile/tiers");
        // Re-read on every activation, not just page load — the ruled uniform
        // door (monetization.md § Pillar 1 → The Tiers-tab re-read door):
        // there is no push kind for a subscribe grant, so a re-click is the
        // only way a subscriber sees an off-box change (an author's approval,
        // a newly published tier).
        if (_vm is not null)
            await _vm.HydrateAsync();
    }

    // ── Header ──────────────────────────────────────────────────────────
    private void CopyActorId_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || string.IsNullOrEmpty(_vm.ActorId)) return;
        var copied = _vm.ActorId;
        FaunaApp.Helpers.ClipboardHelper.CopyText(copied);
        // ui.yaml `profile-actor-id-copy-btn`: the button carries a `copied` attr
        // holding the exact string it put on the clipboard, since no driver reads the
        // OS clipboard. On windows the driver's get_attr maps any non-`disabled` name
        // to HelpText (the SettingsWebPage copy-link buttons' contract). Written AFTER
        // the copy from the same value — a throwing clipboard leaves it unset, so the
        // attr never claims a copy that did not happen.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(CopyActorIdButton, copied);
    }

    /// <summary>Follow (OTHER profile) = subscribe to the free "followers" tier; on
    /// success the button flips to "Following".</summary>
    private async void Follow_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.FollowAsync();
        if (_vm.IsFollowing)
            ApplyFollowLabel(true);
    }

    // ── OTHER-profile secondary relationship actions (start-DM + block) ──

    /// <summary>Start-DM (OTHER profile) — pure client nav glue (profile.md
    /// § Where logic lives → Start DM action): seed the shared Conversations
    /// manager's new-thread composer with this actor as a recipient chip, then
    /// switch to the Conversations page. <c>StartNewConversation</c> +
    /// <c>AcceptNewThreadChip</c> are sync manager mutations (no wire op — the
    /// group bootstraps lazily on first send). The chip carries the real actor_id;
    /// its display handle is the actor_id hex (the OTHER header has no cached
    /// handle, same fallback as the header label). Resolves the SAME manager the
    /// Conversations page renders off — the login session's wired manager in
    /// production, <c>ConversationsManagerHost.Instance</c> in E2E — so the seeded
    /// composer is the one shown after the nav (mirrors linux <c>mod.rs</c>
    /// <c>start_dm</c> + the ConversationsManagerHost fallback in
    /// <c>ConversationsPage.Page_Loaded</c>).</summary>
    private void StartDm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var manager = _clients?.ConvSession?.Manager() ?? ConversationsManagerHost.Instance;
        manager.StartNewConversation();
        try
        {
            var actorBytes = Convert.FromHexString(_vm.ActorId);
            manager.AcceptNewThreadChip(new TypedAddress.Fauna(@handle: _vm.ActorId, @actorId: actorBytes));
        }
        catch (FormatException)
        {
            // A malformed actor id can't seed a chip; still open the (empty) composer.
        }
        MainPage.Current?.NavigateToView("conversations");
    }

    /// <summary>Block ⇄ unblock toggle (OTHER profile) — taps the contact-edge
    /// block/unblock over <c>fauna.knocks.block</c> / <c>fauna.knocks.unblock</c>
    /// (the latter is the guarded clear-the-edge; profile.md § User actions,
    /// contacts.md § Where logic lives → Unblock). The label re-renders from the
    /// new <c>IsBlocked</c> via <see cref="ApplyBlockLabel"/> (the PropertyChanged
    /// case) — Block ⇄ Unblock, no terminal disable. Mirrors linux
    /// <c>mod.rs</c> <c>toggle_block</c>.</summary>
    private async void Block_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.ToggleBlockAsync();
    }

    // ── OTHER-profile offers (subscriber browse) ────────────────────────
    private async void SubscribeOffer_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: SubscriptionOfferRow row }) return;
        await _vm.SubscribeToOfferAsync(row.Name);
        UpdateDerivedUi();
    }

    /// <summary>Open the tier's external checkout URL in the default browser (the
    /// per-row <c>subscription-offer-payment-link</c>; linux <c>offers.rs</c>
    /// UriLauncher) — refused for a non-https scheme via the shared
    /// <c>uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl</c> guard (F-CL2
    /// anti-phishing-redirect class — the same check
    /// <c>UnlockOfferPaymentLink_Click</c> in <c>FeedPage.xaml.cs</c>
    /// applies).</summary>
    private void OfferPaymentLink_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: SubscriptionOfferRow row } || string.IsNullOrEmpty(row.PaymentUrl))
            return;
        if (!uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl(row.PaymentUrl))
        {
            RenderError(S.Get("subscriptions/unsafe_payment_url"));
            return;
        }
        FaunaApp.Services.UrlOpener.Open(row.PaymentUrl, "Profile");
    }

    // ── Profile edit form (display-name / bio / links) ──────────────────
    // The links two-way-bind via EditLinksList.ItemsSource; display-name + bio
    // sync imperatively (the tier-form pattern). No ConfigureAwait(false) — the
    // WinUI bound-state rule.
    private async void EditProfile_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.OpenEditFormAsync();
        SyncEditFormFromVm();
    }

    private void AddLink_Click(object sender, RoutedEventArgs e) => _vm?.AddLink();

    private void RemoveLink_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: ProfileLinkRow row }) return;
        _vm.RemoveLink(row);
    }

    private async void SaveEdit_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        SyncEditFormToVm();
        await _vm.SaveEditAsync(_clients?.Nest);
    }

    private void CancelEdit_Click(object sender, RoutedEventArgs e) => _vm?.CancelEdit();

    /// <summary>Push the VM's edit-form display-name + bio into the TextBoxes (the
    /// links bind via ItemsSource), and reset the avatar/banner buttons to their
    /// localized placeholders (the VM's staged bytes/clear flags were reset by
    /// <c>OpenEditFormAsync</c>). Called after the VM opens + prefills the form.</summary>
    private void SyncEditFormFromVm()
    {
        if (_vm is null) return;
        EditDisplayNameBox.Text = _vm.EditDisplayName;
        EditBioBox.Text = _vm.EditBio;
        AvatarButton.Content = S.Get("profile/edit_avatar");
        BannerButton.Content = S.Get("profile/edit_banner");
    }

    // ── Avatar / banner (profile.md § Where logic lives → Field ownership) ──
    // A single button per field: Content is the localized placeholder until a
    // file is picked, then the picked absolute path (avatar_path_text()/
    // banner_path_text()'s read target — the e2e action layer's contract, per
    // `expected_staged_path_text`). Bytes are read immediately on pick
    // (mirroring FeedComposeBar.ComposeFile_Click's timing); the actual upload
    // defers to Save (ProfileViewModel.SaveEditAsync).

    private async void Avatar_Click(object sender, RoutedEventArgs e)
    {
        if (await PickImageAsync() is not { } picked) return;
        _vm?.StageAvatar(picked.Bytes);
        AvatarButton.Content = picked.Path;
    }

    private async void Banner_Click(object sender, RoutedEventArgs e)
    {
        if (await PickImageAsync() is not { } picked) return;
        _vm?.StageBanner(picked.Bytes);
        BannerButton.Content = picked.Path;
    }

    private void AvatarRemove_Click(object sender, RoutedEventArgs e)
    {
        _vm?.ClearAvatar();
        AvatarButton.Content = S.Get("profile/edit_avatar");
    }

    private void BannerRemove_Click(object sender, RoutedEventArgs e)
    {
        _vm?.ClearBanner();
        BannerButton.Content = S.Get("profile/edit_banner");
    }

    /// <summary>Open a real OS <c>FileOpenPicker</c> filtered to images and read
    /// the picked file's bytes immediately (mirrors
    /// <c>FeedComposeBar.ComposeFile_Click</c>). Returns <c>null</c> on
    /// cancel/failure — the caller leaves the field untouched.</summary>
    private async Task<(byte[] Bytes, string Path)?> PickImageAsync()
    {
        if (App.MainWindow is null) return null;
        try
        {
            var picker = new Windows.Storage.Pickers.FileOpenPicker();
            picker.SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.PicturesLibrary;
            picker.FileTypeFilter.Add(".jpg");
            picker.FileTypeFilter.Add(".jpeg");
            picker.FileTypeFilter.Add(".png");
            picker.FileTypeFilter.Add(".gif");
            picker.FileTypeFilter.Add(".webp");

            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var file = await picker.PickSingleFileAsync();
            if (file is null) return null;

            var buffer = await Windows.Storage.FileIO.ReadBufferAsync(file);
            var bytes = new byte[buffer.Length];
            using var reader = Windows.Storage.Streams.DataReader.FromBuffer(buffer);
            reader.ReadBytes(bytes);
            return (bytes, file.Path);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("ProfilePage", $"avatar/banner pick failed: {ex.Message}");
            return null;
        }
    }

    /// <summary>The TestAgent's <c>compose.file</c> route for
    /// <c>profile-edit-avatar</c>/<c>profile-edit-banner</c> (native bridges can't
    /// drive the real OS file picker — mirrors <see cref="ConversationsPage.StageAttachment"/>).
    /// Reads <paramref name="path"/> from disk and stages it exactly as a real pick
    /// would, echoing the literal path as the button's Content (the e2e action
    /// layer's <c>avatar_path_text()</c>/<c>banner_path_text()</c> read target).
    /// Returns <c>false</c> if the form isn't open or the target is unrecognized.</summary>
    internal bool StageImageFromTestAgent(string target, byte[] bytes, string path)
    {
        if (_vm is null || !_vm.ShowEditForm) return false;
        switch (target)
        {
            case "profile-edit-avatar":
                _vm.StageAvatar(bytes);
                AvatarButton.Content = path;
                return true;
            case "profile-edit-banner":
                _vm.StageBanner(bytes);
                BannerButton.Content = path;
                return true;
            default:
                return false;
        }
    }

    /// <summary>Pull the display-name + bio TextBoxes into the VM before
    /// <c>SaveEditAsync</c> (the links are already two-way-bound).</summary>
    private void SyncEditFormToVm()
    {
        if (_vm is null) return;
        _vm.EditDisplayName = EditDisplayNameBox.Text;
        _vm.EditBio = EditBioBox.Text;
    }

    // ── §1 My tiers ─────────────────────────────────────────────────────
    private void CreateTier_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.OpenCreateForm();
        SyncFormFromVm();
    }

    private void EditTier_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: SubscriptionTierRow row }) return;
        _vm.OpenEditForm(row);
        SyncFormFromVm();
    }

    private void CancelForm_Click(object sender, RoutedEventArgs e) => _vm?.CancelForm();

    private async void SaveForm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        SyncFormToVm();
        await _vm.SaveFormAsync();
        UpdateDerivedUi();
    }

    private async void DeleteTier_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: SubscriptionTierRow row }) return;
        await _vm.DeleteTierAsync(row.Name);
        UpdateDerivedUi();
    }

    // ── §2 Pending requests ─────────────────────────────────────────────
    private async void ApproveRequest_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: SubscriptionRequestRow row }) return;
        await _vm.ApproveAsync(row.RequestId);
        UpdateDerivedUi();
    }

    private async void RejectRequest_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: SubscriptionRequestRow row }) return;
        await _vm.RejectAsync(row.RequestId);
        UpdateDerivedUi();
    }

    // ── §3 Subscribers roster ───────────────────────────────────────────
    private async void TierSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_syncingTierSelect || _vm is null) return;
        if (TierSelect.SelectedItem is string name)
        {
            await _vm.SelectTierAsync(name);
            UpdateDerivedUi();
        }
    }

    private async void RemoveSubscriber_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: SubscriptionSubscriberRow row }) return;
        await _vm.RemoveSubscriberAsync(row.SubscriberId, _vm.SelectedTier);
        UpdateDerivedUi();
    }

    // ── render helpers ──────────────────────────────────────────────────

    /// <summary>Refresh the placeholder visibilities + sync the §3 tier picker
    /// selection. Called after the initial load and every mutation (the VM re-reads
    /// internally; this reflects the new collection state, observer-free).</summary>
    private void UpdateDerivedUi()
    {
        if (_vm is null) return;
        NoTiersText.Visibility = _vm.Tiers.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        NoRequestsText.Visibility = _vm.Requests.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        NoSubscribersText.Visibility = _vm.Subscribers.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        NoOffersText.Visibility = _vm.Offers.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        TierForm.Visibility = _vm.ShowForm ? Visibility.Visible : Visibility.Collapsed;
        RequestBusyText.Visibility = _vm.IsApproving ? Visibility.Visible : Visibility.Collapsed;
        SyncTierSelect();
#if PAYMENTS
        _payments?.UpdateDerivedUi();
#endif
    }

    /// <summary>Reflect the VM's <c>SelectedTier</c> into the picker without echoing
    /// back a roster-refresh dispatch (the <c>_syncingTierSelect</c> guard).</summary>
    private void SyncTierSelect()
    {
        if (_vm is null) return;
        var target = string.IsNullOrEmpty(_vm.SelectedTier) ? null : _vm.SelectedTier;
        if (Equals(TierSelect.SelectedItem, target)) return;
        _syncingTierSelect = true;
        TierSelect.SelectedItem = target;
        _syncingTierSelect = false;
    }

    /// <summary>Push the VM's form state into the create/edit form fields (the name
    /// is read-only while editing — the server key). Called after the VM opens the
    /// form in create or edit mode.</summary>
    private void SyncFormFromVm()
    {
        if (_vm is null) return;
        FormNameBox.Text = _vm.FormName;
        FormRankBox.Text = _vm.FormRank;
        FormDescriptionBox.Text = _vm.FormDescription;
        FormPriceHintBox.Text = _vm.FormPriceHint;
        FormPaymentUrlBox.Text = _vm.FormPaymentUrl;
        FormAutoApproveToggle.IsOn = _vm.FormAutoApprove;
        FormNameBox.IsReadOnly = _vm.EditingTier is not null;
#if PAYMENTS
        if (_tierAskingPriceInput is not null) _tierAskingPriceInput.Text = _vm.FormAskingPriceSats;
#endif
    }

    /// <summary>Pull the form fields into the VM before <c>SaveFormAsync</c>.</summary>
    private void SyncFormToVm()
    {
        if (_vm is null) return;
        _vm.FormName = FormNameBox.Text;
        _vm.FormRank = FormRankBox.Text;
        _vm.FormDescription = FormDescriptionBox.Text;
        _vm.FormPriceHint = FormPriceHintBox.Text;
        _vm.FormPaymentUrl = FormPaymentUrlBox.Text;
        _vm.FormAutoApprove = FormAutoApproveToggle.IsOn;
#if PAYMENTS
        if (_tierAskingPriceInput is not null) _vm.FormAskingPriceSats = _tierAskingPriceInput.Text;
#endif
    }

    /// <summary>Render the VM's error onto the app-wide <c>error-message</c> InfoBar
    /// + the state-protocol error surface (mirrors AdminCalendarPage).</summary>
    private void RenderError(string? msg)
    {
        var empty = string.IsNullOrEmpty(msg);
        ErrorBar.Message = msg ?? string.Empty;
        ErrorBar.IsOpen = !empty;
        ErrorTextMirror.Text = msg ?? " ";
        App.CurrentErrorMessage = empty ? null : msg;
    }
}

/// <summary>Maps a bool to <see cref="Visibility"/> (Visible when true) — the
/// per-row <c>subscription-offer-payment-link</c> shows only when the tier carries a
/// checkout URL.</summary>
public sealed class BoolToVisibilityConverter : Microsoft.UI.Xaml.Data.IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) =>
        value is true ? Visibility.Visible : Visibility.Collapsed;

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}
