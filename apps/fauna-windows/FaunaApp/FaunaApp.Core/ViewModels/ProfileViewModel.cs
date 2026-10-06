using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Profile page's view model — the canonical per-user SELF detail surface
/// (<c>profile.md</c>) hosting the subscriptions Slice-A author management on the
/// Tiers tab (<c>monetization.md</c> § Pillar 1). Mirrors apple
/// <c>SubscriptionsVM</c> / linux <c>tiers.rs</c>: observer-free, a manual re-read
/// after each mutation (no client-side caching — <c>feed.md</c> § Architectural
/// rules). Three SELF sections render over the <see cref="INestRpcClient"/> seam:
/// §1 My tiers (CRUD), §2 Pending requests (approve-with-mint / reject), §3
/// Subscribers (roster + remove-with-mint). The three mint-bearing actions go
/// through the shared author orchestration (the encrypted-mode KeyBlob mint+upload
/// lives in shared Rust, priority #2); plaintext mode mints nest-side behind the
/// same calls. Rich identity (display name / avatar / bio) is publish-path-gated,
/// so the header renders the actor id (the handle fallback).
///
/// <para>No <c>ConfigureAwait(false)</c> on any await — bound-state mutation must
/// resume on the UI thread (the WinUI VM rule).</para>
/// </summary>
public partial class ProfileViewModel : ViewModelBase
{
    private readonly INestRpcClient _nest;

    /// <summary>The viewed actor's id (hex) — for SELF the viewer's own, for OTHER the
    /// target. The header's <c>profile-handle</c> fallback + the
    /// <c>profile-actor-id-copy-btn</c> target until a published display-name lands.</summary>
    public string ActorId { get; }

    /// <summary>Whether this is the viewer's own profile. Drives the header primary
    /// action (<c>profile-edit-button</c> vs <c>profile-follow-button</c>) and the
    /// Tiers-tab branch (SELF author management vs OTHER offers browse —
    /// <c>profile.md</c> § Layout &amp; flow).</summary>
    public bool IsSelf { get; }

    // ── OTHER-profile offers browse (monetization.md § Pillar 1 surface 2) ───
    /// The free "followers" tier — followed via the header <c>profile-follow-button</c>,
    /// so it is excluded from the per-row paid offers (linux <c>offers.rs</c>).
    private const string FollowersTier = "followers";
    /// <summary>The creator's offered tiers (OTHER profile), excluding "followers".</summary>
    public ObservableCollection<SubscriptionOfferRow> Offers { get; } = new();
    /// <summary><c>true</c> once the viewer follows this creator (the header
    /// <c>profile-follow-button</c> flips its label to "Following").</summary>
    [ObservableProperty] private bool _isFollowing;
    /// <summary><c>true</c> when the viewer has a <c>blocked</c> contact edge with
    /// this user — drives the <c>profile-block-button</c> Block⇄Unblock toggle label
    /// (Unblock when blocked, Block otherwise). Read on OTHER-profile open from
    /// <c>fauna.contacts.list</c> (<see cref="RefreshBlockStateAsync"/>) and flipped
    /// by each successful <see cref="ToggleBlockAsync"/>; mirrors linux
    /// <c>views/profile/mod.rs</c> <c>is_blocked</c>.</summary>
    [ObservableProperty] private bool _isBlocked;

    // §1 My tiers
    public ObservableCollection<SubscriptionTierRow> Tiers { get; } = new();
    [ObservableProperty] private bool _showForm;
    [ObservableProperty] private string? _editingTier;
    [ObservableProperty] private string _formName = "";
    [ObservableProperty] private string _formRank = "";
    [ObservableProperty] private string _formDescription = "";
    [ObservableProperty] private string _formPriceHint = "";
    /// <summary>The gated <c>subscription-tier-form-asking-price</c> input's raw text
    /// (monetization.md § The asking price) — an inert data field, ungated by design:
    /// dynamic-features.md § A gated plane's user-facing INPUTS excise with it says
    /// only the RENDER excises, so a store-safe build's <see cref="OpenEditForm"/>
    /// still pre-fills this from the tier's current price and <see cref="SaveFormAsync"/>
    /// still sends it back unchanged — the "preserve with no carry-through code" property.
    /// Whole sats, free text; empty or unparseable both mean no machine price (never an
    /// error at this layer — mirrors tui's `profile/mod.rs` reference leg exactly).</summary>
    [ObservableProperty] private string _formAskingPriceSats = "";
    [ObservableProperty] private string _formPaymentUrl = "";
    [ObservableProperty] private bool _formAutoApprove;

    // §2 Pending requests
    public ObservableCollection<SubscriptionRequestRow> Requests { get; } = new();
    [ObservableProperty] private bool _isApproving;
    private IReadOnlyList<FfiPendingRequest> _rawRequests = new List<FfiPendingRequest>();

    // §3 Subscribers roster
    public ObservableCollection<string> TierNames { get; } = new();
    [ObservableProperty] private string _selectedTier = "";
    public ObservableCollection<SubscriptionSubscriberRow> Subscribers { get; } = new();

#if PAYMENTS
    // §4 Payment providers (monetization.md § Pillar 3, IDs reserved 2026-07-12)
    public ObservableCollection<PaymentProviderRow> Providers { get; } = new();
    [ObservableProperty] private bool _showProviderForm;
    [ObservableProperty] private string _providerFormKind = "";
    [ObservableProperty] private string _providerFormSecret = "";
    [ObservableProperty] private string _providerFormTier = "";
    [ObservableProperty] private string _providerWebhookUrl = "";

    // §5 Manual claim codes (monetization.md § Pillar 3)
    public ObservableCollection<PaymentClaimRow> Claims { get; } = new();
    [ObservableProperty] private string _claimTier = "";
#endif   // PAYMENTS

    // ── Profile edit (display-name / bio / links read-modify-write) ──────────
    /// <summary>The header label — the published display name when present, else the
    /// <see cref="ActorId"/> fallback (rich identity is publish-path-gated).</summary>
    [ObservableProperty] private string _headerName = "";
    [ObservableProperty] private bool _showEditForm;
    [ObservableProperty] private string _editDisplayName = "";
    [ObservableProperty] private string _editBio = "";
    /// <summary>The editable links rows — XAML two-way-binds each row's Label/Uri.</summary>
    public ObservableCollection<ProfileLinkRow> EditLinks { get; } = new();
    /// The opaque stored body the read fetched — the read-modify-write base the save
    /// hands back to <c>build_edited_profile_with_images</c> so the non-display
    /// fields survive.
    private byte[]? _baseBody;

    // ── Avatar / banner (profile.md § Where logic lives → Field ownership) ──
    // The page owns the OS file picker + reads the bytes immediately on pick
    // (mirroring FeedComposeBar's SetAttachment timing, not linux's deferred
    // path-only staging) and hands them here; the actual HTTP upload defers to
    // Save (FeedPage.OnComposePostRequested's same timing) so a cancelled edit
    // never uploads. A fresh pick always overrides a pending clear.
    private byte[]? _stagedAvatarBytes;
    [ObservableProperty] private bool _editAvatarClear;
    private byte[]? _stagedBannerBytes;
    [ObservableProperty] private bool _editBannerClear;

    /// <summary>Stage a freshly-picked avatar's bytes, overriding any pending clear.</summary>
    public void StageAvatar(byte[] bytes)
    {
        _stagedAvatarBytes = bytes;
        EditAvatarClear = false;
    }

    /// <summary>Flag the avatar for removal (<c>profile-edit-avatar-remove-button</c>) —
    /// a subsequent fresh pick still overrides this at Save.</summary>
    public void ClearAvatar()
    {
        _stagedAvatarBytes = null;
        EditAvatarClear = true;
    }

    /// <summary>Stage a freshly-picked banner's bytes, overriding any pending clear.</summary>
    public void StageBanner(byte[] bytes)
    {
        _stagedBannerBytes = bytes;
        EditBannerClear = false;
    }

    /// <summary>Flag the banner for removal (<c>profile-edit-banner-remove-button</c>).</summary>
    public void ClearBanner()
    {
        _stagedBannerBytes = null;
        EditBannerClear = true;
    }

    /// <summary>The SELF profile (own actor id) — the <c>profile-tab</c> entry.</summary>
    internal ProfileViewModel(INestRpcClient nest, string actorId)
        : this(nest, actorId, isSelf: true) { }

    /// <summary>The viewed profile. <paramref name="isSelf"/> <c>false</c> = another
    /// actor's profile (tap-through), rendering the OTHER header (follow button) + the
    /// Tiers-tab offers browse instead of the SELF author management.</summary>
    internal ProfileViewModel(INestRpcClient nest, string actorId, bool isSelf)
    {
        _nest = nest;
        ActorId = actorId;
        IsSelf = isSelf;
        HeaderName = actorId;   // the actor-id fallback until a refresh lands a name
    }

    /// <summary>Load the Tiers-tab data: SELF → re-read §1 tiers + §2 requests, the §3
    /// picker (preserving the prior selection by name) + roster; OTHER → the creator's
    /// offered tiers + this viewer's status (<see cref="LoadOffersAsync"/>). Runs on
    /// mount and after every mutation (observer-free).</summary>
    public async Task HydrateAsync()
    {
        ErrorMessage = null;
        if (!IsSelf)
        {
            await LoadOffersAsync();
            return;
        }
        try
        {
            var tiers = await _nest.SubscriptionTiersListAsync();
            var requests = await _nest.SubscriptionRequestsListAsync();
#if PAYMENTS
            var providers = await _nest.PaymentsProvidersListAsync();
            var claims = await _nest.PaymentsClaimsListAsync();
#endif

            // §1 My tiers excludes designated/sold-post tiers (monetization.md § Per-post
            // pay-to-unlock: "hidden from generic tier surfaces" — a machine-named single-post
            // tier is a purchase, never a real subscription tier). Client-side off
            // `unlocksPost`, same shape as linux's `tiers.rs::render_tier_rows`. §3/§4/§5's
            // `TierNames` picker below stays UNFILTERED — a designated tier still needs a
            // claim minted / subscribers viewed against it.
            Tiers.Clear();
            foreach (var t in tiers.Where(t => t.unlocksPost is null))
                Tiers.Add(new SubscriptionTierRow(
                    t.name, t.rank, t.description, t.priceHint, t.paymentUrl, t.autoApprove,
                    t.askingPriceSats));

            _rawRequests = requests;
            Requests.Clear();
            foreach (var r in requests)
                Requests.Add(new SubscriptionRequestRow(
                    r.requestId, Hex(r.subscriberId), r.tierName, r.kind, r.paymentEntitled));

#if PAYMENTS
            Providers.Clear();
            foreach (var p in providers)
                Providers.Add(new PaymentProviderRow(p.kind, p.tier, p.lastVerifiedAt, p.lastRejectedAt));

            // Shared 3-state decision (redeemed wins over voided) —
            // fauna_core::format::claim_status_label; the single source every
            // app resolves so a row can never disagree across clients.
            Claims.Clear();
            foreach (var c in claims)
                Claims.Add(new PaymentClaimRow(
                    c.code, c.tier,
                    Strings.Resolve(FaunaFfiMethods.ClaimStatusLabel(c.redeemedBy is not null, c.voidedAt is not null))));
#endif

            // Repopulate §3/§4/§5 tier pickers, preserving each prior selection by
            // name (else first) — mirrors linux's provider_tier_names/claim_tier_names.
            var prevTier = SelectedTier;
#if PAYMENTS
            var prevProviderTier = ProviderFormTier;
            var prevClaimTier = ClaimTier;
#endif
            TierNames.Clear();
            foreach (var t in tiers)
                TierNames.Add(t.name);
            SelectedTier = TierNames.Contains(prevTier) ? prevTier : (TierNames.FirstOrDefault() ?? "");
#if PAYMENTS
            ProviderFormTier = TierNames.Contains(prevProviderTier) ? prevProviderTier : (TierNames.FirstOrDefault() ?? "");
            ClaimTier = TierNames.Contains(prevClaimTier) ? prevClaimTier : (TierNames.FirstOrDefault() ?? "");
#endif

            await RefreshRosterAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Read the selected tier's roster and re-render §3. A no-op (clears the
    /// roster) when no tier is selected.</summary>
    public async Task RefreshRosterAsync()
    {
        if (string.IsNullOrEmpty(SelectedTier))
        {
            Subscribers.Clear();
            return;
        }
        try
        {
            var roster = await _nest.SubscriptionSubscribersListAsync(SelectedTier);
            Subscribers.Clear();
            foreach (var s in roster)
                Subscribers.Add(new SubscriptionSubscriberRow(Hex(s.subscriberId), s.subscriberId));
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── §1 form ──────────────────────────────────────────────────────────

    public void OpenCreateForm()
    {
        EditingTier = null;
        FormName = "";
        FormRank = "";
        FormDescription = "";
        FormPriceHint = "";
        FormPaymentUrl = "";
        FormAutoApprove = false;
        FormAskingPriceSats = "";
        ErrorMessage = null;
        ShowForm = true;
    }

    public void OpenEditForm(SubscriptionTierRow tier)
    {
        EditingTier = tier.Name;            // the name is the server key (read-only while editing)
        FormName = tier.Name;
        FormRank = tier.Rank.ToString();
        FormDescription = tier.Description ?? "";
        FormPriceHint = tier.PriceHint ?? "";
        FormPaymentUrl = tier.PaymentUrl ?? "";
        FormAutoApprove = tier.AutoApprove;
        // Pre-fill from the tier's current price so leaving the field UNCHANGED
        // round-trips it, and leaving it EMPTY (the field cleared) sends `null`,
        // which `tiers.update` reads as "keep current" — never "clear"
        // (monetization.md § The asking price → Editability; no client exposes
        // `tiers.clear_field` yet).
        FormAskingPriceSats = tier.AskingPriceSats?.ToString() ?? "";
        ErrorMessage = null;
        ShowForm = true;
    }

    public void CancelForm()
    {
        EditingTier = null;
        ShowForm = false;
    }

    /// <summary>Create or update the tier depending on <see cref="EditingTier"/>,
    /// then refresh. A blank name is a no-op. Create routes through the author
    /// orchestration (period-key custody); update is a thin tier-edit.</summary>
    public async Task SaveFormAsync()
    {
        var name = FormName.Trim();
        if (name.Length == 0)
            return;
        var rank = uint.TryParse(FormRank.Trim(), out var r) ? r : 0u;
        var description = Opt(FormDescription);
        var priceHint = Opt(FormPriceHint);
        var paymentUrl = Opt(FormPaymentUrl);
        var auto = FormAutoApprove;
        // Empty or unparseable both mean no machine price — never an error at this
        // layer (mirrors tui's `non_empty(&form.asking_price).and_then(|s|
        // s.parse::<u64>().ok())` exactly). The FFI call itself refuses (throws) a
        // value too large to express once converted to msats — caught below like
        // any other RPC fault, never silently truncated.
        var askingPriceSats = ulong.TryParse(FormAskingPriceSats.Trim(), out var sats)
            ? sats
            : (ulong?)null;
        ErrorMessage = null;
        try
        {
            if (EditingTier is not null)
                await _nest.SubscriptionTierUpdateAsync(name, rank, description, priceHint, paymentUrl, auto, askingPriceSats);
            else
                await _nest.SubscriptionTierCreateAsync(name, rank, description, priceHint, paymentUrl, auto, askingPriceSats);
            ShowForm = false;
            EditingTier = null;
            await HydrateAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    public async Task DeleteTierAsync(string name)
    {
        ErrorMessage = null;
        try
        {
            await _nest.SubscriptionTierDeleteAsync(name);
            await HydrateAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── §2 requests ──────────────────────────────────────────────────────

    /// <summary>Approve a pending request — the transparent mint+upload; shows the
    /// busy indicator (<c>subscription-request-busy</c>) for its duration. Looks the
    /// raw <c>FfiPendingRequest</c> up by id (the orchestration keys the mint off the
    /// subscriber + tier).</summary>
    public async Task ApproveAsync(long requestId)
    {
        var request = _rawRequests.FirstOrDefault(r => r.requestId == requestId);
        if (request is null)
            return;
        ErrorMessage = null;
        IsApproving = true;
        try
        {
            await _nest.SubscriptionRequestApproveAsync(request);
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsApproving = false;
        }
        await HydrateAsync();
    }

    public async Task RejectAsync(long requestId)
    {
        ErrorMessage = null;
        try
        {
            await _nest.SubscriptionRequestRejectAsync(requestId);
            await HydrateAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── §3 roster ────────────────────────────────────────────────────────

    public async Task SelectTierAsync(string name)
    {
        SelectedTier = name;
        await RefreshRosterAsync();
    }

    /// <summary>Remove a subscriber from <paramref name="tierName"/> — rotates the
    /// period key + re-mints over the reduced roster.</summary>
    public async Task RemoveSubscriberAsync(byte[] subscriberId, string tierName)
    {
        ErrorMessage = null;
        try
        {
            await _nest.SubscriptionSubscriberRemoveAsync(tierName, subscriberId);
            await RefreshRosterAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

#if PAYMENTS
    // Gated as one unit with the state above — see the §4/§5 fields.
    // ── §4 Payment providers (monetization.md § Pillar 3) ──────────────────

    /// <summary>The provider kinds the §4 form's kind select enumerates — the
    /// shared <c>fauna-payments</c> registry the nest validates against (pure,
    /// no round trip).</summary>
    public IReadOnlyList<string> KnownProviderKinds => FaunaFfiMethods.PaymentsKnownKinds();

    partial void OnProviderFormKindChanged(string value) => UpdateWebhookUrlPreview();

    /// <summary>Recompute the §4 form's live webhook-URL preview from the
    /// current kind selection — the exact URL to register at the provider's
    /// dashboard (<c>fauna_payments::webhook_url</c>, FFI
    /// <c>PaymentsWebhookUrl</c> — single-sourced with the nest's own route so
    /// client and nest can never disagree). No nest round-trip.</summary>
    private void UpdateWebhookUrlPreview() =>
        ProviderWebhookUrl = FaunaFfiMethods.PaymentsWebhookUrl(_nest.HomeUrl, ActorId, ProviderFormKind);

    /// <summary>Open the §4 add form — the secret is never pre-filled (the
    /// nest doesn't echo it back; a re-save re-enters it).</summary>
    public void OpenProviderForm()
    {
        ProviderFormSecret = "";
        ProviderFormKind = KnownProviderKinds.FirstOrDefault() ?? "";
        ProviderFormTier = TierNames.FirstOrDefault() ?? "";
        UpdateWebhookUrlPreview();
        ErrorMessage = null;
        ShowProviderForm = true;
    }

    public void CancelProviderForm() => ShowProviderForm = false;

    /// <summary>Save the §4 form (<c>fauna.payments.providers.set</c>). No
    /// client-side validation — the nest rejects unknown kinds / dangling
    /// tiers / empty secrets with typed <c>fauna.payments.*</c> errors that
    /// surface via <see cref="ShowError"/>. A blank kind/tier (no tiers yet)
    /// is a no-op.</summary>
    public async Task SaveProviderFormAsync()
    {
        if (ProviderFormKind.Length == 0 || ProviderFormTier.Length == 0)
            return;
        ErrorMessage = null;
        try
        {
            await _nest.PaymentsProvidersSetAsync(ProviderFormKind, ProviderFormSecret, ProviderFormTier);
            ShowProviderForm = false;
            // The secret is a credential — don't leave it bound after the nest has it.
            ProviderFormSecret = "";
            await HydrateAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Remove a provider config (<c>fauna.payments.providers.remove</c>
    /// — idempotent).</summary>
    public async Task RemoveProviderAsync(string kind)
    {
        ErrorMessage = null;
        try
        {
            await _nest.PaymentsProvidersRemoveAsync(kind);
            await HydrateAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    // ── §5 Manual claim codes (monetization.md § Pillar 3) ─────────────────

    /// <summary>Mint a manual claim code for <see cref="ClaimTier"/>
    /// (<c>fauna.payments.claims.mint</c>; provider is always
    /// <c>"manual"</c> nest-side). A blank tier (no tiers yet) is a no-op.</summary>
    public async Task MintClaimAsync()
    {
        if (ClaimTier.Length == 0)
            return;
        ErrorMessage = null;
        try
        {
            await _nest.PaymentsClaimsMintAsync(ClaimTier, null);
            await HydrateAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

#endif   // PAYMENTS

    // ── OTHER-profile offers browse ──────────────────────────────────────
    // The profile Tiers-tab OTHER section (monetization.md § Pillar 1 surface 2;
    // profile.md § Layout & flow → Another's profile). Reads over shared Rust: the
    // creator's offered tiers (offers.list) + this viewer's status (status.get),
    // and subscribe on the per-row button. Observer-free: a manual re-read after
    // each subscribe. Mirrors apps/fauna-linux/src/views/profile/offers.rs.

    /// <summary>Load the creator's offered tiers (excluding the free "followers" tier
    /// — that is the header follow button) + this viewer's status, and render the
    /// offer rows. A failed status read is non-fatal (every row renders "not
    /// subscribed"); a failed offers read surfaces the error.</summary>
    public async Task LoadOffersAsync()
    {
        ErrorMessage = null;
        try
        {
            var author = AuthorIdBytes();
            var offers = await _nest.SubscriptionOffersListAsync(author);
            string? heldTier;
            try
            {
                heldTier = (await _nest.SubscriptionStatusGetAsync(author)).tier;
            }
            catch
            {
                heldTier = null;   // non-fatal — every row renders "not subscribed"
            }
            Offers.Clear();
            foreach (var t in offers)
            {
                if (t.name == FollowersTier)
                    continue;   // the header follow button's job, not a paid row
                var status = FaunaFfiMethods.OfferStatus(t.name, heldTier, false);
                Offers.Add(new SubscriptionOfferRow(t.name, t.priceHint, t.description, t.paymentUrl, status));
            }
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Subscribe the viewer to <paramref name="tierName"/> (the per-row
    /// Subscribe). <c>Approved</c> (plaintext / auto-approve) → re-read so the row
    /// shows Active; <c>Queued</c> (encrypted) → the row flips to Pending in place (no
    /// re-read — <c>status.get</c> only reports the confirmed tier, so a reload would
    /// show None; linux <c>offers.rs</c> <c>subscribe_to</c>).</summary>
    public async Task SubscribeToOfferAsync(string tierName)
    {
        ErrorMessage = null;
        try
        {
            var reply = await _nest.SubscriptionSubscribeAsync(AuthorIdBytes(), tierName);
            switch (reply)
            {
                case FfiSubscribeReply.Approved:
                    await LoadOffersAsync();
                    break;
                case FfiSubscribeReply.Queued:
                    var row = Offers.FirstOrDefault(o => o.Name == tierName);
                    if (row is not null)
                        // Transient post-click Pending: status.get carries no pending
                        // discriminant, so the viewer doesn't hold tierName — a null
                        // status_tier + pending=true resolves to Pending (precedence).
                        row.Status = FaunaFfiMethods.OfferStatus(tierName, null, true);
                    break;
            }
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Follow = subscribe to the free "followers" tier (<c>profile.md</c>
    /// § Where logic lives → Follow). On success the header button flips to
    /// "Following" (<see cref="IsFollowing"/>).</summary>
    public async Task FollowAsync()
    {
        ErrorMessage = null;
        try
        {
            await _nest.SubscriptionSubscribeAsync(AuthorIdBytes(), FollowersTier);
            IsFollowing = true;
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Read the viewed actor's current contact edge
    /// (<c>fauna.contacts.list</c>) and set <see cref="IsBlocked"/> from it — a
    /// <c>blocked</c> edge for this actor → the toggle reads "Unblock", any
    /// other/absent edge leaves the synchronous "Block" default. Runs on
    /// OTHER-profile open so the initial label reflects the relationship
    /// (<c>profile.md</c> § User actions; the contact-edge roster carries
    /// <c>status</c> — <c>contacts.md</c> § Persistence). A read failure is
    /// non-fatal — it leaves the default (mirrors linux <c>refresh_block_state</c>'s
    /// <c>Err(_) =&gt; false</c>).</summary>
    public async Task RefreshBlockStateAsync()
    {
        try
        {
            var contacts = await _nest.ContactsListAsync();
            // Shared predicate (fauna_core::format::contact_row_blocks_actor; FFI
            // ContactRowBlocksActor) — a blocked edge whose peer_id matches this actor
            // (case-insensitive hex). The wire status string is the lowercased enum
            // name. contacts.md § Where logic lives → Unblock.
            IsBlocked = contacts.Any(c =>
                FaunaFfiMethods.ContactRowBlocksActor(
                    c.ActorId, c.Status.ToString().ToLowerInvariant(), ActorId));
        }
        catch (Exception)
        {
            // Non-fatal — leave the synchronous "Block" default (linux Err(_) => false).
        }
    }

    /// <summary>Block ⇄ unblock toggle (OTHER profile) — the same contact-edge
    /// client <c>contacts.md</c> owns; the profile is a caller, not a second
    /// implementation. On a not-blocked edge the tap upserts the edge to
    /// <c>blocked</c> via the shared <c>fauna_client_contacts::knocks_block</c> over
    /// <c>fauna.knocks.block</c>; on a blocked edge it calls <c>knocks_unblock</c>
    /// over <c>fauna.knocks.unblock</c> (the guarded clear-the-edge,
    /// <c>ContactStatus</c> → <c>None</c>; <c>contacts.md</c> § Where logic lives →
    /// Unblock). On success <see cref="IsBlocked"/> flips and the label re-renders
    /// (Block ⇄ Unblock); a failure surfaces via <see cref="ShowError"/> and leaves
    /// the state unchanged for a retry. Mirrors linux <c>views/profile/mod.rs</c>
    /// <c>toggle_block</c>.</summary>
    public async Task ToggleBlockAsync()
    {
        ErrorMessage = null;
        var wasBlocked = IsBlocked;
        try
        {
            if (wasBlocked)
                await _nest.KnocksUnblockAsync(ActorId);
            else
                await _nest.KnocksBlockAsync(ActorId);
            IsBlocked = !wasBlocked;
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// The viewed actor id as 32 raw bytes (the subscribe/offers/status seam shape).
    private byte[] AuthorIdBytes() => Convert.FromHexString(ActorId);

    // ── Profile edit form ────────────────────────────────────────────────
    // The cross-app profile-edit form (profile.md § State & data shape). The
    // open fetches the current display fields + opaque base body (read-modify-write
    // base); save signs+publishes via the shared build_edited_profile, preserving
    // the non-display fields. Mirrors apps/fauna-linux/src/views/profile/edit.rs.

    /// <summary>Open the edit form, prefilling the three display fields from the
    /// current profile (an unpublished profile / get failure → an empty form, the
    /// first-publish path). Clones the fetched links into fresh rows so live edits
    /// don't mutate the fetched model.</summary>
    public async Task OpenEditFormAsync()
    {
        ErrorMessage = null;
        try
        {
            var res = await _nest.LoadProfileEditBaseAsync();
            _baseBody = res?.RawBody;
            EditDisplayName = res?.Display.DisplayName ?? "";
            EditBio = res?.Display.Bio ?? "";
            EditLinks.Clear();
            if (res?.Display.Links is { } links)
                foreach (var l in links)
                    EditLinks.Add(new ProfileLinkRow(l.Label, l.Uri));
            _stagedAvatarBytes = null;
            EditAvatarClear = false;
            _stagedBannerBytes = null;
            EditBannerClear = false;
            ShowEditForm = true;
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Append a blank link row to the edit form (the linux "add link"
    /// gesture).</summary>
    public void AddLink() => EditLinks.Add(new ProfileLinkRow("", ""));

    /// <summary>Remove a link row from the edit form.</summary>
    public void RemoveLink(ProfileLinkRow row) => EditLinks.Remove(row);

    /// <summary>Sign + publish the edited display fields + avatar/banner via the
    /// shared read-modify-write (non-display fields preserved from the base body),
    /// close the form, and refresh the header. Fully-empty link rows (both label and
    /// uri blank) are dropped, mirroring linux <c>collect_links</c>. A staged
    /// picture upload failure fails the whole save rather than publishing text-only
    /// (mirrors linux <c>submit</c>'s "never drop a picture the user picked" rule) —
    /// errors of any kind surface via <c>ShowError</c> and keep the form open.
    /// <paramref name="http"/> is the HTTP blob-upload plane, resolved by the
    /// caller per call (never captured — <see cref="NestRpcClient"/>'s own
    /// "resolve per call" rule); <c>null</c> is only safe when no picture was
    /// staged this open (every existing text-only call site).</summary>
    public async Task SaveEditAsync(INestHttpClient? http = null)
    {
        var links = EditLinks
            .Where(r => r.Label.Trim().Length != 0 || r.Uri.Trim().Length != 0)
            .ToList();
        var display = new ProfileDisplay(Opt(EditDisplayName), Opt(EditBio), links);
        ErrorMessage = null;
        try
        {
            var avatar = await ResolveImageEditAsync(_stagedAvatarBytes, EditAvatarClear, http, "avatar").ConfigureAwait(true);
            var banner = await ResolveImageEditAsync(_stagedBannerBytes, EditBannerClear, http, "banner").ConfigureAwait(true);
            await _nest.ProfileSetAsync(display, _baseBody, avatar, banner);
            ShowEditForm = false;
            await RefreshHeaderAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Resolve one image field's staged state into the
    /// <see cref="FfiProfileImageEdit"/> <c>ProfileSetAsync</c> needs — a fresh pick
    /// (non-null <paramref name="stagedBytes"/>) always wins over a pending clear,
    /// which wins over untouched (mirrors linux <c>pending_image</c>'s priority).
    /// Uploads via the shared public-post blob path (media.md § Encryption at rest:
    /// avatar/banner are "the same shape" as post attachments — signed plaintext,
    /// no seal).</summary>
    private static async Task<FfiProfileImageEdit> ResolveImageEditAsync(
        byte[]? stagedBytes, bool clear, INestHttpClient? http, string fieldName)
    {
        if (stagedBytes is not null)
        {
            if (http is null)
                throw new InvalidOperationException($"{fieldName}: no nest client to upload the picked image");
            var hashHex = await http.UploadBlobAsync(stagedBytes, new UploadAudience.PublicPost()).ConfigureAwait(true);
            return new FfiProfileImageEdit.Set(hashHex);
        }
        return clear ? new FfiProfileImageEdit.Clear() : new FfiProfileImageEdit.Keep();
    }

    /// <summary>Discard the edit form without publishing.</summary>
    public void CancelEdit() => ShowEditForm = false;

    /// <summary>Re-read the profile and update <see cref="HeaderName"/> — the
    /// published display name when present, else the <see cref="ActorId"/> fallback
    /// (an unpublished profile / read failure keeps the fallback).</summary>
    public async Task RefreshHeaderAsync()
    {
        try
        {
            var res = await _nest.ProfileGetAsync(ActorId);
            HeaderName = !string.IsNullOrWhiteSpace(res?.Display.DisplayName)
                ? res!.Display.DisplayName!
                : ActorId;
        }
        catch (Exception)
        {
            HeaderName = ActorId;
        }
    }

    /// Lowercase hex of a 32-byte actor id (the display form linux/apple use; rich
    /// identity is publish-path-gated — <c>profile.md</c>).
    private static string Hex(byte[] bytes) => FaunaFfiMethods.HexFull(bytes);

    /// Trim + collapse a blank field to <c>null</c> (the optional wire shape),
    /// mirroring linux <c>opt</c> / apple <c>optTrim</c>.
    private static string? Opt(string s)
    {
        var t = s.Trim();
        return t.Length == 0 ? null : t;
    }
}
