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
using FaunaApp.Feed;
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
public sealed partial class ProfilePage : Page, IPostCardPage
{
    /// <summary>The live instance, set on load / cleared on unload — the TestAgent's
    /// <c>compose.file</c> route for <c>profile-edit-avatar</c>/<c>profile-edit-banner</c>
    /// (native bridges can't drive the real OS file picker; mirrors
    /// <c>ConversationsPage.Current</c>/<c>StageAttachment</c>). The agent is its only
    /// reader, so it compiles out of Release with it (convention 15).</summary>
#if DEBUG || FAUNA_E2E_AGENT
    internal static ProfilePage? Current { get; private set; }
#endif

    private ProfileViewModel? _vm;

#if PAYMENTS
    /// <summary>The §§4-5 payments surface, built in code and dropped into
    /// <c>PaymentsSectionsHost</c>. Not named from markup: its type is removed from the
    /// store-safe build (FaunaApp.csproj drops Views\Payments\**), and a XAML element
    /// naming it would fail to compile there. Null until the first
    /// <c>Page_Loaded</c>.</summary>
    private Views.Payments.PaymentsAuthorSections? _payments;

    /// <summary>The tier editor's three money fields, dropped into
    /// <c>TierMoneyFieldsHost</c> — same removable-item reasoning as
    /// <see cref="_payments"/> (dynamic-features.md § Platform-family surface excision
    /// → The price-and-route class). Null until the first <c>Page_Loaded</c>.</summary>
    private Views.Payments.TierMoneyFields? _tierMoneyFields;
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
        _cards = new PostCardHost(this, ProfilePostsList, "profile", TipListDialogHost);
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
            var targetHome = App.PendingProfileHome;
            App.PendingProfileHome = null;
            var isSelf = string.IsNullOrEmpty(target);
            var actorId = isSelf
                ? (clients.Crypto.HasKey ? clients.Crypto.ActorIdHex : "")
                : target!;
            // The SELF header's handle is read at every paint, never baked here: the
            // launch machine's live identity over the account's registry cache, through
            // the shared resolver (mail-primary-domain-rename.md § Display-handle
            // interaction — a rename moves it).
            var account = clients.Account;
            _vm = new ProfileViewModel(clients.Rpc, actorId, isSelf, targetHome,
                selfDisplayHandle: isSelf
                    ? () =>
                    {
                        var live = App.CurrentLaunchIdentity;
                        return SelfDisplayHandle.Resolve(
                            live?.handle, live?.domain, account.Handle, account.Domain);
                    }
                    : null);
            PublishProfileState();
            _vm.PropertyChanged += ViewModel_PropertyChanged;

            // Rich identity is publish-path-gated, so until the profile read lands the
            // header has no display name: SELF shows the handle on the current primary,
            // else the actor id; OTHER the shared resolver's answer — the viewer's
            // nickname, else the canonical short id (never a windows-side fallback).
            _vm.RepaintHeader();
            HandleText.Text = _vm.HeaderName;
            ApplyPublicName();
            PageHeading.Text = S.Get("profile/posts");

            // Header primary action (edit vs follow) + Tiers-tab branch (author
            // management vs offers browse) follow the is_self split.
            ApplyProfileMode(isSelf);

            // The private section's editor: one per open of one OTHER person's profile,
            // over the manager as it stands now (profile.md § The private section).
            _private.Reset();
            if (!isSelf && !string.IsNullOrEmpty(actorId))
            {
                _private.Open(actorId);
                PaintPrivate();
            }
        }
    }

    // ── Private section (profile.md § The private section) ─────────────────────
    private readonly ProfilePrivateViewModel _private = new();

    // Set while a programmatic text assignment syncs a field to the editor, so
    // reflecting state never echoes back as a staging gesture.
    private bool _syncingPrivate;

    /// <summary>The overlay projection moved (this device's Save, a sibling device's
    /// edit, a late load after a relaunch): re-ask the header's two names, and let an
    /// UNTOUCHED section follow the live overlay (the editor's own staging rule).
    /// Raised off the UI thread — marshalled by the caller.</summary>
    private void OnOverlayMoved() => DispatcherQueue.TryEnqueue(() =>
    {
        _vm?.RepaintHeader();
        if (_private.Reload()) PaintPrivate();
    });

    /// <summary>The account's identity moved (the launch machine's verify reply named a
    /// new handle or primary): the SELF header re-reads its handle with no profile
    /// re-read. Raised on the machine's thread — marshalled here.</summary>
    private void OnSelfIdentityMoved() => DispatcherQueue.TryEnqueue(() =>
    {
        if (_vm is { IsSelf: true }) _vm.RepaintHeader();
    });

    /// <summary><c>profile-public-name</c>: the public name a nickname replaced —
    /// collapsed (absent from UIA) while no nickname is the primary line.</summary>
    private void ApplyPublicName()
    {
        var name = _vm?.HeaderPublicName ?? "";
        PublicNameText.Text = name;
        PublicNameText.Visibility = name.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>Paint the section from the editor: the two text fields (only where the
    /// text differs, so typing never fights the caret) and the label chips.</summary>
    private void PaintPrivate()
    {
        _syncingPrivate = true;
        try
        {
            if (PrivateNicknameBox.Text != _private.Nickname) PrivateNicknameBox.Text = _private.Nickname;
            if (PrivateNotesBox.Text != _private.Notes) PrivateNotesBox.Text = _private.Notes;
        }
        finally { _syncingPrivate = false; }

        PrivateLabelList.Children.Clear();
        for (var i = 0; i < _private.Labels.Count; i++)
        {
            var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            // AutomationId ONLY, no static Name — get_text reads the label itself
            // (reference_windows_flaui_gettext_dynamic_label_no_name).
            var chip = new TextBlock { Text = _private.Labels[i], VerticalAlignment = VerticalAlignment.Center };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(chip, Ids.ProfileLabelChip);
            var remove = new Button
            {
                Content = S.Get("profile/private_label_remove"),
                Tag = i,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(remove, Ids.ProfileLabelRemoveButton);
            remove.Click += PrivateLabelRemove_Click;
            row.Children.Add(chip);
            row.Children.Add(remove);
            PrivateLabelList.Children.Add(row);
        }
        // The staged set moved, so the offered set did too.
        PaintPrivateSuggestions();
    }

    /// <summary>Repaint the completion rows under the add field: none until something is
    /// typed; a pick stages the label exactly as if typed and added.</summary>
    private void PaintPrivateSuggestions()
    {
        PrivateLabelSuggestions.Children.Clear();
        foreach (var suggestion in _private.LabelSuggestions(PrivateLabelBox.Text))
        {
            var row = new Button { Content = suggestion, Tag = suggestion };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(row, Ids.ProfileLabelSuggestion);
            row.Click += PrivateLabelSuggestion_Click;
            PrivateLabelSuggestions.Children.Add(row);
        }
    }

    private void PrivateLabel_TextChanged(object sender, TextChangedEventArgs e) => PaintPrivateSuggestions();

    private void PrivateLabelSuggestion_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string label })
        {
            if (_private.AddLabel(label)) PrivateLabelBox.Text = "";
            RenderError(_private.ErrorMessage);
            PaintPrivate();
        }
    }

    private void PrivateNickname_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_syncingPrivate) return;
        _private.SetNickname(PrivateNicknameBox.Text);
    }

    private void PrivateNotes_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_syncingPrivate) return;
        _private.SetNotes(PrivateNotesBox.Text);
    }

    private void PrivateLabelAdd_Click(object sender, RoutedEventArgs e)
    {
        // A staged label empties the add field; a refused one keeps the text and shows
        // its refusal on error-message.
        if (_private.AddLabel(PrivateLabelBox.Text)) PrivateLabelBox.Text = "";
        RenderError(_private.ErrorMessage);
        PaintPrivate();
    }

    private void PrivateLabelRemove_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: int index })
        {
            _private.RemoveLabel(index);
            PaintPrivate();
        }
    }

    private async void PrivateSave_Click(object sender, RoutedEventArgs e)
    {
        // No ConfigureAwait(false): the paint after the await must stay on the UI thread.
        await _private.SaveAsync();
        RenderError(_private.ErrorMessage);
        PaintPrivate();
        // The Save reloads the projection before it returns: the header re-reads the
        // names at once rather than waiting for the observer wake.
        _vm?.RepaintHeader();
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
        // The private section never renders on the viewer's own profile.
        PrivateSection.Visibility = isSelf ? Visibility.Collapsed : Visibility.Visible;
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
        _payments.Attach(_vm, _clients?.Account.Handle);

        _tierMoneyFields = new Views.Payments.TierMoneyFields();
        TierMoneyFieldsHost.Content = _tierMoneyFields;
#endif

#if DEBUG || FAUNA_E2E_AGENT
        Current = this;
#endif
        ContactOverlayNames.Moved += OnOverlayMoved;
        SelfDisplayHandle.Moved += OnSelfIdentityMoved;
        // The ward's durable asks moved (a status read landed): repaint the ask pair.
        WardAsksCache.Changed += OnWardAsksChanged;
        PaintKnock();

        // The Posts tab is the default tab: the page's open is an open of the tab, so it
        // reads the author's posts now, beside the header and Tiers reads below.
        _ = OpenPostsWindowAsync();

        // Render the header's two names on load: the shared resolver's answer over the
        // published display_name (the HeaderName / HeaderPublicName changes push through
        // ViewModel_PropertyChanged).
        await _vm.RefreshHeaderAsync();
        PublishProfileState();
        HandleText.Text = _vm.HeaderName;
        ApplyPublicName();

        // The OTHER header holds no handle: re-read the contact roster now, so a
        // contact accepted a moment ago is named by the roster's handle rather than
        // the short id until the receive loop's next tick (value-formatting.md § Peer
        // display label → the roster handle; apple's ProfileView open task is the
        // twin). The projection's move also raises ContactOverlayNames.Moved; the
        // repaint here does not wait for that wake.
        if (!_vm.IsSelf)
        {
            await ContactOverlayNames.RefreshRosterAsync();
            _vm.RepaintHeader();
        }

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
            ApplyBlockEnabled();
        }
    }

    private void Page_Unloaded(object sender, RoutedEventArgs e)
    {
#if DEBUG || FAUNA_E2E_AGENT
        if (ReferenceEquals(Current, this)) Current = null;
#endif
        App.ProfileStateNow = null;
        ContactOverlayNames.Moved -= OnOverlayMoved;
        SelfDisplayHandle.Moved -= OnSelfIdentityMoved;
        WardAsksCache.Changed -= OnWardAsksChanged;
        // An open still in flight lands nowhere; the window stops observing.
        _postsEpoch++;
        DetachPostsWindow();
    }

    private void OnWardAsksChanged() => DispatcherQueue.TryEnqueue(PaintKnock);

    /// <summary>Resolved per call (never captured) — the nest client is replaced on re-auth;
    /// <see cref="Helpers.BlobImageLoader"/> documents the staleness it prevents.</summary>
    private Helpers.BlobImageLoader? _pictureLoader;

    /// <summary>Bumped per paint; a plane build that finishes after a newer paint began
    /// does not bind (the VM's own epoch drops stale reads, this drops stale binds).</summary>
    private int _pictureEpoch;

    /// <summary>Paint <c>profile-avatar</c> / <c>profile-banner</c> from the VM: an element
    /// is rendered only while the profile names its picture, and
    /// <see cref="Helpers.ImageHashBind"/> answers <c>get_attr(state)</c> —
    /// <c>placeholder</c> until the bytes are on screen, <c>painted</c> after; a failed
    /// fetch leaves it a placeholder, never <c>error-message</c>. The post-blob plane is
    /// built before the first bind so a relayed profile's bytes never fall back to this
    /// nest's by-hash GET.</summary>
    private async void PaintPictures()
    {
        if (_vm is null) return;
        var epoch = ++_pictureEpoch;
        var avatar = _vm.AvatarHash;
        var banner = _vm.BannerHash;
        var home = _vm.PictureHome;
        if (avatar is not null || banner is not null)
        {
            try
            {
                if (_clients?.Rpc is { } rpc) await Core.PostBlobHostsHost.GetOrBuildAsync(rpc);
            }
            catch (Exception ex)
            {
                // No plane: this nest's pictures still load through the by-hash GET, a
                // relayed one stays a placeholder (the loader treats it as a miss).
                ShellLog.Warn("ProfilePage", $"post-blob plane unavailable for header pictures: {ex.Message}");
            }
            if (epoch != _pictureEpoch) return;
        }
        _pictureLoader ??= new Helpers.BlobImageLoader(() => App.CurrentNest);
        BindPicture(AvatarImage, avatar, home);
        BindPicture(BannerImage, banner, home);
        AvatarGlyph.Visibility = avatar is null ? Visibility.Visible : Visibility.Collapsed;
    }

    private void BindPicture(Image image, string? hash, object? home)
    {
        Helpers.ImageHashBind.SetManageVisibility(image, false);
        Helpers.ImageHashBind.SetLoader(image, _pictureLoader);
        Helpers.ImageHashBind.SetHome(image, home);
        Helpers.ImageHashBind.SetHash(image, hash);
        image.Visibility = hash is null ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>Paint <c>profile-request-contact-button</c> and the guardian-ask pair
    /// from the VM (OTHER profile only; the row is collapsed on SELF). The knock reads
    /// "Request sent" and stays disabled once sent or in flight; the pair shows the
    /// durable pending label first, else the ask after a TYPED refusal this open saw,
    /// else nothing — no supervision test here (rule (h)).</summary>
    private void PaintKnock()
    {
        if (_vm is null || _vm.IsSelf) return;
        var ask = _vm.KnockAsk;
        RequestContactButton.Content = S.Get(ask.KnockSent ? "profile/request_contact_sent" : "profile/request_contact");
        RequestContactButton.IsEnabled = !ask.KnockSent && !ask.KnockInFlight;
        var render = _vm.ContactAskRenderNow();
        AskGuardianButton.Visibility = render == ContactAskRender.Ask ? Visibility.Visible : Visibility.Collapsed;
        AskGuardianButton.IsEnabled = !ask.AskInFlight;
        AskPendingText.Visibility = render == ContactAskRender.Pending ? Visibility.Visible : Visibility.Collapsed;
    }

    private async void RequestContact_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.RequestContactAsync();
    }

    private async void AskGuardian_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.AskGuardianAsync();
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

    /// <summary>Hold the <c>profile-block-button</c> disabled until the open-time
    /// contact-edge read lands (<see cref="ProfileViewModel.IsBlockKnown"/>), so a press
    /// cannot re-send <c>knocks_block</c> to an actor the viewer already blocks (tui parity).</summary>
    private void ApplyBlockEnabled()
    {
        if (_vm is null) return;
        BlockButton.IsEnabled = _vm.IsBlockKnown;
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
            case nameof(ProfileViewModel.HeaderPublicName):
                ApplyPublicName();
                break;
            case nameof(ProfileViewModel.AvatarHash):
            case nameof(ProfileViewModel.BannerHash):
            case nameof(ProfileViewModel.PictureHome):
                PaintPictures();
                break;
            case nameof(ProfileViewModel.KnockAsk):
                PaintKnock();
                break;
            case nameof(ProfileViewModel.IsBlocked):
                ApplyBlockLabel();
                break;
            case nameof(ProfileViewModel.IsBlockKnown):
                ApplyBlockEnabled();
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
        // Every activation reads the author's posts afresh (profile.md § Persistence —
        // no client cache), as the Tiers tab's own re-read door does.
        _ = OpenPostsWindowAsync();
    }

    private async void TiersTab_Click(object sender, RoutedEventArgs e)
    {
        PostsPanel.Visibility = Visibility.Collapsed;
        TiersPanel.Visibility = Visibility.Visible;
        PageHeading.Text = S.Get("profile/tiers");
        // The window's read error belongs to the Posts tab alone.
        ShowPostsError(null);
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
        // A retired session (a click landing after an actor change, before the frame
        // takes this page away) starts nothing.
        if (ConversationsManagerHost.ManagerFor(_clients?.ConvSession, "start a DM") is not { } manager)
            return;
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

    /// <summary>Report the viewed account (OTHER profile; moderation.md §
    /// User-initiated reporting → App surface) through the shared report sheet. The
    /// target is the shared <c>report_actor_target</c>: an account is not sealed, so
    /// the sheet offers no excerpt checkbox. A landed report is acknowledged OUTSIDE
    /// the closed sheet (<c>report-status</c>) and has already stored the
    /// reporter-side hide; a ticked block-author re-reads the block toggle so it
    /// reads Unblock. A failed send keeps the sheet and reports on
    /// <c>error-message</c>; a failed block/hide lands there BESIDE the
    /// acknowledgement.</summary>
    private async void Report_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || _clients?.Rpc is not { } rpc) return;
        ReportStatusText.Visibility = Visibility.Collapsed;
        RenderError(null);
        var target = uniffi.fauna_ffi.FaunaFfiMethods.ReportActorTarget(_vm.ActorId);
        var outcome = await Controls.ReportSheetDialog.OpenAsync(
            this.XamlRoot, rpc, target, msg => RenderError(msg));
        if (outcome is null) return;
        ReportStatusText.Text = outcome.Acknowledgement ?? "";
        ReportStatusText.Visibility = Visibility.Visible;
        RenderError(outcome.FollowUpError);
        if (outcome.BlockedAuthor is not null) await _vm.RefreshBlockStateAsync();
    }

    // ── OTHER-profile offers (subscriber browse) ────────────────────────
    private async void SubscribeOffer_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not FrameworkElement { Tag: SubscriptionOfferRow row }) return;
        await _vm.SubscribeToOfferAsync(row.Name);
        UpdateDerivedUi();
    }

    // ── The price-and-route class's per-row renders (dynamic-features.md § Platform-family
    //    surface excision → The price-and-route class) ─────────────────────────────────
    // Each handler is declared unconditionally with only its body under PAYMENTS: a
    // DataTemplate event handler is resolved against this class at XAML-compile time and
    // XAML carries no #if, so hiding the whole method would break the store-safe build the
    // excision exists to produce (FeedPage.TipDisplayHost_Loaded's shape). Loaded fires
    // once per realized row, and both lists are rebuilt with Clear + Add, so every row
    // arrives in a fresh container.

    /// <summary>Fill one tier row's price column (<see cref="Views.Payments.TierPrice"/>).</summary>
    private void TierPriceHost_Loaded(object sender, RoutedEventArgs e)
    {
#if PAYMENTS
        if (sender is not ContentControl { DataContext: SubscriptionTierRow row } host) return;
        if (host.Content is not Views.Payments.TierPrice price)
            host.Content = price = new Views.Payments.TierPrice();
        price.Bind(row);
#endif   // PAYMENTS
    }

    /// <summary>Fill one offer row's price line (<see cref="Views.Payments.SubscriptionOfferPriceText"/>).</summary>
    private void OfferPriceHost_Loaded(object sender, RoutedEventArgs e)
    {
#if PAYMENTS
        if (sender is not ContentControl { DataContext: SubscriptionOfferRow row } host) return;
        if (host.Content is not Views.Payments.SubscriptionOfferPriceText price)
            host.Content = price = new Views.Payments.SubscriptionOfferPriceText();
        price.Bind(row);
#endif   // PAYMENTS
    }

    /// <summary>Fill one offer row's checkout link (<see cref="Views.Payments.OfferPaymentLink"/>).</summary>
    private void OfferPaymentLinkHost_Loaded(object sender, RoutedEventArgs e)
    {
#if PAYMENTS
        if (sender is not ContentControl { DataContext: SubscriptionOfferRow row } host) return;
        if (host.Content is not Views.Payments.OfferPaymentLink link)
        {
            link = new Views.Payments.OfferPaymentLink();
            link.OpenRequested += (_, r) => OpenOfferPaymentLink(r);
            host.Content = link;
        }
        link.Bind(row);
#endif   // PAYMENTS
    }

#if PAYMENTS
    /// <summary>Open the tier's external checkout URL in the default browser (the per-row
    /// checkout link; linux <c>offers.rs</c> UriLauncher) — refused for a non-https scheme
    /// via the shared <c>uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl</c> guard
    /// (F-CL2 anti-phishing-redirect class — the same check
    /// <c>FeedPage.OpenUnlockOfferPaymentLink</c> applies).</summary>
    private void OpenOfferPaymentLink(SubscriptionOfferRow row)
    {
        if (string.IsNullOrEmpty(row.PaymentUrl)) return;
        if (!uniffi.fauna_core.FaunaCoreMethods.IsSafePaymentUrl(row.PaymentUrl))
        {
            RenderError(S.Get("subscriptions/unsafe_payment_url"));
            return;
        }
        FaunaApp.Services.UrlOpener.Open(row.PaymentUrl, "Profile");
    }
#endif

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

    /// <summary>True while <see cref="SyncEditFormFromVm"/> paints the toggle, so the
    /// paint is not read back as a user press.</summary>
    private bool _syncingAudience;

    private void EditAudienceToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_vm is null || _syncingAudience) return;
        _vm.SetAudience(EditAudienceToggle.IsOn);
    }

    /// <summary>Publish the viewed actor + actor-home slot to the <c>data.profile</c>
    /// state read (<c>profile.actor_id</c> / <c>profile.home_nest_url</c>).</summary>
    private void PublishProfileState()
    {
        if (_vm is null) return;
        App.ProfileStateNow = (_vm.ActorId, _vm.ActorHomeUrl);
    }

    /// <summary>Push the VM's edit-form display-name + bio into the TextBoxes (the
    /// links bind via ItemsSource), and reset the avatar/banner buttons to their
    /// localized placeholders (the VM's staged bytes/clear flags were reset by
    /// <c>OpenEditFormAsync</c>). Called after the VM opens + prefills the form.</summary>
    private void SyncEditFormFromVm()
    {
        if (_vm is null) return;
        EditDisplayNameBox.Text = _vm.EditDisplayName;
        EditBioBox.Text = _vm.EditBio;
        _syncingAudience = true;
        EditAudienceToggle.IsOn = _vm.EditAudienceEveryone;
        _syncingAudience = false;
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

#if DEBUG || FAUNA_E2E_AGENT
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
#endif

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

    // ── Posts tab (profile.md § Layout & flow → The Posts tab) ──────────────────
    //
    // The tab is a feed read narrowed to one author, adding no post logic of its own:
    // an AUTHOR WINDOW of the session's feed manager (FfiFeedManager.AuthorWindow —
    // feed.md § State & data shape → The author window) behind a window-bound
    // FeedViewModel, its rows the feed's own post-card template, and the cards' verbs
    // the feed's own card behaviour (PostCardHost) acting on the window. A card's
    // activation is the door a search result uses: the feed page's post_detail over
    // the timeline's resolve_post. Mirrors tui's profile::posts, linux's
    // views/profile/posts.rs and apple's ProfilePostsSection.

    /// <summary>The Posts tab's cards — the feed page's own card behaviour, acting on
    /// <see cref="_postsVm"/> through <see cref="IPostCardPage"/>.</summary>
    private readonly PostCardHost _cards;

    /// <summary>The window behind the tab, minted afresh on every open; null before the
    /// first open lands and after the page unloads.</summary>
    private FeedViewModel? _postsVm;

    /// <summary>Bumped per open and at unload: an open that finishes after a newer one
    /// began (or after the page left) mints nothing and paints nothing.</summary>
    private int _postsEpoch;

    /// <summary>The window's read error while it is the one on <c>error-message</c> —
    /// so a later tick clears only what this tab painted, never the page's other errors.</summary>
    private string? _postsErrorShown;

    /// <summary>A card verb's failure while it is the one on <c>error-message</c> — the
    /// feed page's sticky page error, which the next verb that lands clears.</summary>
    private string? _cardErrorShown;

    /// <summary>Open the tab: mint a fresh author window off the session's feed manager
    /// for the actor on screen (the viewer's own on SELF) and read its first page. Called
    /// on the page's open and on every <c>profile-posts-tab</c> activation, so each open
    /// reads afresh. The feed manager is built here when the Feed page has not been
    /// opened this session (FeedManagerHost is the one per-session manager), and the
    /// post-blob plane and the room-post key seam are in place before the window is
    /// minted, so its cards' pictures and room labels resolve as on the feed.</summary>
    private async Task OpenPostsWindowAsync()
    {
        if (_vm is null || _clients?.Rpc is not { } rpc || string.IsNullOrEmpty(_vm.ActorId)) return;
        var clients = _clients;
        var actorId = _vm.ActorId;
        var epoch = ++_postsEpoch;
        try
        {
            var timeline = await Core.FeedManagerHost.GetOrBuildAsync(
                rpc, () => rpc.BuildFeedManagerAsync(clients.Account?.ResolveDeviceId()));
            await Core.PostBlobHostsHost.GetOrBuildAsync(rpc);
            // The window shares the seams installed on the timeline it is minted from;
            // install the room-post seam first (idempotent — FeedPage.Page_Loaded's own).
            if (clients.ConvSession is not null) timeline.SetRoomPostKeys(clients.ConvSession);
            if (epoch != _postsEpoch) return;

            DetachPostsWindow();
            var window = FeedViewModel.ForAuthorWindow(
                timeline.AuthorWindow(actorId), new FeedNotifyObserver(), rpc);
            window.PropertyChanged += PostsWindow_PropertyChanged;
            _postsVm = window;
            // The previous open's rows go at once: nothing renders while the first read
            // is in flight (profile.md § The Posts tab → Empty, loading, error).
            RefreshPosts();
            await window.ReloadCommand.ExecuteAsync(null);
            if (ReferenceEquals(_postsVm, window)) RefreshPosts();
        }
        catch (Exception ex)
        {
            if (epoch != _postsEpoch) return;
            ShellLog.Warn(nameof(ProfilePage), $"posts window open failed: {ex.GetType().Name}: {ex.Message}");
            ShowPostsError(S.Error(ex));
        }
    }

    private void PostsWindow_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (ReferenceEquals(sender, _postsVm)) RefreshPosts();
    }

    /// <summary>Stop observing the current window (a newer open replaces it, or the page
    /// left). Its manager is the window's own, so its observer list is the VM's to clear.</summary>
    private void DetachPostsWindow()
    {
        if (_postsVm is not { } old) return;
        _postsVm = null;
        old.PropertyChanged -= PostsWindow_PropertyChanged;
        old.DetachFromManager();
        old.CleanupReconnect();
        old.CleanupStoreChange();
    }

    /// <summary>Repaint the tab from the window's snapshot: the rows (the feed's own
    /// reconcile and lazy resolves), <c>profile-posts-empty</c> off the shared
    /// <c>FeedSnapshot::empty_state</c>, the load-more footer while the window holds
    /// more, and the window's read error on <c>error-message</c>.</summary>
    private void RefreshPosts()
    {
        var window = _postsVm;
        _cards.Sync(window?.Posts ?? Array.Empty<uniffi.fauna_feed.PostSummary>());
        NoPostsText.Visibility = window?.EmptyState == uniffi.fauna_feed.FeedEmptyState.NoPosts
            ? Visibility.Visible : Visibility.Collapsed;
        LoadMorePostsButton.Visibility = window is { HasMore: true, Posts.Count: > 0 }
            ? Visibility.Visible : Visibility.Collapsed;
        ShowPostsError(window?.ErrorText);
    }

    /// <summary>The window's read error on the page's <c>error-message</c>, while the
    /// Posts tab is showing (tui's <c>posts::page_error</c>); its clearing clears only
    /// what this tab painted.</summary>
    private void ShowPostsError(string? error)
    {
        if (!string.IsNullOrEmpty(error) && PostsPanel.Visibility == Visibility.Visible)
        {
            if (error == _postsErrorShown) return;
            _postsErrorShown = error;
            RenderError(error);
            return;
        }
        if (_postsErrorShown is null) return;
        if (App.CurrentErrorMessage == _postsErrorShown) RenderError(null);
        _postsErrorShown = null;
    }

    /// <summary>The tab's next page — the feed page's own load-more button.</summary>
    private async void LoadMorePosts_Click(object sender, RoutedEventArgs e)
    {
        if (_postsVm is not { } window) return;
        await _cards.RunVerbAsync("load more", window.LoadMoreAsync);
    }

    // ── IPostCardPage — what the Posts tab's cards act on and paint on ──────────

    FeedViewModel? IPostCardPage.CardViewModel => _postsVm;
    INestRpcClient? IPostCardPage.Rpc => _clients?.Rpc;
    ICryptoService? IPostCardPage.Crypto => _clients?.Crypto;
    ISessionAccount? IPostCardPage.Account => _clients?.Account;

    void IPostCardPage.ReportPageError(string message)
    {
        _cardErrorShown = message;
        RenderError(message);
    }

    void IPostCardPage.ClearPageError()
    {
        if (_cardErrorShown is null) return;
        if (App.CurrentErrorMessage == _cardErrorShown) RenderError(null);
        _cardErrorShown = null;
    }

    void IPostCardPage.ShowError(string message) => RenderError(message);

    void IPostCardPage.ShowReportStatus(string? acknowledgement)
    {
        ReportStatusText.Text = acknowledgement ?? "";
        ReportStatusText.Visibility = acknowledgement is null ? Visibility.Collapsed : Visibility.Visible;
    }

    void IPostCardPage.RefreshCards() => RefreshPosts();

    /// <summary>Open the page's lightbox overlay (<c>image-lightbox</c>) on a card's
    /// already-loaded picture.</summary>
    public void ShowImageLightbox(Microsoft.UI.Xaml.Media.Imaging.BitmapImage image) => Lightbox.Show(image);

    /// <summary>A card's activation: the feed page's <c>post_detail</c> through the door a
    /// search result uses (search.md § User actions → SearchNav.Post; profile.md § The
    /// Posts tab → What a row is) — the feed page opens the post from what its timeline
    /// holds, else through the timeline manager's <c>resolve_post</c>. A REPOST ROW opens
    /// its original, as on the feed.</summary>
    Task IPostCardPage.OpenPostDetailAsync(FeedPostItem item)
    {
        if (_clients is null) return Task.CompletedTask;
        var postId = item.IsRepostRow && item.RepostedPostId is { Length: > 0 } original
            ? original
            : item.PostId;
        Frame.Navigate(typeof(FeedPage), _clients with { DeepLinkPostId = postId });
        return Task.CompletedTask;
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
        FormAutoApproveToggle.IsOn = _vm.FormAutoApprove;
        FormNameBox.IsReadOnly = _vm.EditingTier is not null;
#if PAYMENTS
        if (_tierMoneyFields is not null)
        {
            _tierMoneyFields.PriceHint = _vm.FormPriceHint;
            _tierMoneyFields.AskingPriceSats = _vm.FormAskingPriceSats;
            _tierMoneyFields.PaymentUrl = _vm.FormPaymentUrl;
        }
#endif
    }

    /// <summary>Pull the form fields into the VM before <c>SaveFormAsync</c>. A store-safe
    /// build has no money fields to pull, so the VM keeps the price hint, payment URL and
    /// asking price the form was opened with — an edited priced tier keeps its price
    /// (dynamic-features.md § Platform-family surface excision → The price-and-route class,
    /// omit-means-keep).</summary>
    private void SyncFormToVm()
    {
        if (_vm is null) return;
        _vm.FormName = FormNameBox.Text;
        _vm.FormRank = FormRankBox.Text;
        _vm.FormDescription = FormDescriptionBox.Text;
        _vm.FormAutoApprove = FormAutoApproveToggle.IsOn;
#if PAYMENTS
        if (_tierMoneyFields is not null)
        {
            _vm.FormPriceHint = _tierMoneyFields.PriceHint;
            _vm.FormAskingPriceSats = _tierMoneyFields.AskingPriceSats;
            _vm.FormPaymentUrl = _tierMoneyFields.PaymentUrl;
        }
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
/// per-row <c>subscription-request-paid-badge</c> shows only when the request was
/// paid through a configured provider.</summary>
public sealed class BoolToVisibilityConverter : Microsoft.UI.Xaml.Data.IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) =>
        value is true ? Visibility.Visible : Visibility.Collapsed;

    public object ConvertBack(object value, Type targetType, object parameter, string language) =>
        throw new NotSupportedException();
}
