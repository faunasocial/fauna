using CommunityToolkit.Mvvm.ComponentModel;

namespace FaunaApp.Core.Models;

/// <summary>
/// One of the author's own subscription tiers, projected for the profile Tiers
/// tab §1 "My tiers" list (<c>docs/goal/behavior/monetization.md</c> § Pillar 1;
/// <c>subscription-tier-list</c> rows). Carries every editable field so the
/// inline create/edit form (<c>subscription-tier-form</c>) round-trips a row
/// without a second fetch — mirrors apple <c>FfiTierItem</c> consumption +
/// linux <c>build_tier_row</c>. Projected from the internal UniFFI
/// <c>FfiTierItem</c> (the authenticated <c>tiers.list</c> own-read).
/// </summary>
public sealed record SubscriptionTierRow(
    string Name,
    uint Rank,
    string? Description,
    string? PriceHint,
    string? PaymentUrl,
    bool AutoApprove,
    /// <summary>The machine-comparable purchase threshold in whole sats
    /// (monetization.md § The asking price) — an inert data field, ungated:
    /// only the form's RENDER excises in a store-safe build
    /// (dynamic-features.md § A gated plane's user-facing INPUTS excise with
    /// it), never this projection.</summary>
    ulong? AskingPriceSats = null);

/// <summary>
/// One pending subscribe/unsubscribe request for the author's tiers, projected
/// for the profile Tiers tab §2 (<c>subscription-request-list</c> rows).
/// <see cref="Subscriber"/> is the lowercase-hex actor id (rich identity is
/// publish-path-gated — <c>profile.md</c> § State &amp; data shape). The VM keeps
/// the raw <c>FfiPendingRequest</c> internally (keyed by <see cref="RequestId"/>)
/// for the approve mint+upload, so this row stays pure-public for XAML binding.
/// <see cref="PaymentEntitled"/> renders the §2 <c>subscription-request-paid-badge</c>
/// (<c>monetization.md</c> § Pillar 3 — <c>FfiPendingRequest.paymentEntitled</c>).
/// </summary>
public sealed record SubscriptionRequestRow(
    long RequestId,
    string Subscriber,
    string TierName,
    string Kind,
    bool PaymentEntitled);

/// <summary>
/// One subscriber of the selected tier, projected for the profile Tiers tab §3
/// roster (<c>subscription-subscriber-list</c> rows). <see cref="Handle"/> is the
/// lowercase-hex actor id (publish-path-gated rich identity); <see cref="SubscriberId"/>
/// is the 32-byte actor id the per-row remove passes to the rotate+re-mint.
/// </summary>
public sealed record SubscriptionSubscriberRow(
    string Handle,
    byte[] SubscriberId);

/// <summary>
/// One of the consumer's own active subscriptions, projected for the
/// <c>subscription-settings</c> page (<c>subscription-mine-list</c> rows).
/// Mirrors apple <c>FfiMineSubscription</c> consumption and linux
/// <c>build_mine_row</c>. <see cref="AuthorDisplay"/> is the author's handle when
/// present, else lowercase-hex of the 32-byte actor id (publish-path-gated rich
/// identity). <see cref="Status"/> is the RAW wire string ("active"/"pending") —
/// never translated or capitalised; a tier_3 e2e asserts the literal text.
/// </summary>
public sealed record SubscriptionMineRow(
    byte[] AuthorId,
    string AuthorDisplay,
    string Tier,
    string Status,
    ulong Since);

// The viewer's per-tier status (<c>subscription-offer-status</c>) is the SHARED
// `fauna_core::format::OfferStatus { None, Pending, Active }` enum, consumed over the
// value-format FFI face (`uniffi.fauna_core.OfferStatus`) — the single source the five
// apps converged on (profile.md § Where logic lives → Tiers tab; monetization.md
// § Pillar 1). Derived via `FaunaFfiMethods.OfferStatus(tier, status_tier, pending)`
// (precedence Active>Pending>None) + localized via `FaunaFfiMethods.OfferStatusLabel`.
// No local twin — the prior `OfferStatusKind` enum was folded into the shared one.

/// <summary>
/// One tier a creator offers, as seen by a prospective subscriber — the profile
/// Tiers-tab OTHER-profile <c>subscription-offer-list</c> row
/// (<c>monetization.md</c> § Pillar 1 surface 2; <c>profile.md</c> § Layout &amp; flow
/// → Another's profile). Projected from the internal UniFFI <c>FfiTierItem</c> (the
/// <c>offers.list</c> read), excluding the free "followers" tier (that is the header
/// <c>profile-follow-button</c>). An <see cref="ObservableObject"/> so the per-row
/// <see cref="Status"/> flips to <c>OfferStatus.Pending</c> on an
/// encrypted-mode <c>Queued</c> subscribe without a full re-read (linux
/// <c>offers.rs</c> <c>subscribe_to</c>). The display fields are immutable.
/// </summary>
public partial class SubscriptionOfferRow : ObservableObject
{
    public string Name { get; }
    public string? PriceHint { get; }
    public string? Description { get; }
    public string? PaymentUrl { get; }

    /// <summary>Whether to show the external <c>subscription-offer-payment-link</c>
    /// button (only when the tier carries a checkout URL — linux <c>offers.rs</c>).</summary>
    public bool HasPaymentUrl => !string.IsNullOrEmpty(PaymentUrl);

    // The shared `fauna_core::format::OfferStatus` is an INTERNAL uniffi type, so it
    // can't back a public property/ctor (CS0053/CS0051). Keep it in an internal
    // observable `Status` (the VM derives + sets it; tests assert it over
    // InternalsVisibleTo) and expose the resolved badge label publicly for XAML —
    // the ContactInfo.StatusLabel / AttendeeInfo.RsvpLabel get-only pattern (no
    // per-app converter or label map; the shared offer_status_label is the source).
    private uniffi.fauna_core.OfferStatus _status;
    internal uniffi.fauna_core.OfferStatus Status
    {
        get => _status;
        set
        {
            if (SetProperty(ref _status, value))
                OnPropertyChanged(nameof(StatusLabel));
        }
    }

    /// <summary>The localized <c>subscription-offer-status</c> badge text, resolved from
    /// the shared <c>offer_status_label</c> (FFI <c>OfferStatusLabel</c> →
    /// <c>subscriptions.offer_status_*</c>). Re-raised when <see cref="Status"/> flips
    /// (the encrypted-mode <c>Queued</c> overlay).</summary>
    public string StatusLabel =>
        FaunaApp.Core.Services.Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.OfferStatusLabel(_status));

    internal SubscriptionOfferRow(
        string name, string? priceHint, string? description, string? paymentUrl, uniffi.fauna_core.OfferStatus status)
    {
        Name = name;
        PriceHint = priceHint;
        Description = description;
        PaymentUrl = paymentUrl;
        _status = status;
    }
}

#if PAYMENTS
// ── The payments plane's row projections. Gated with the glue that fills them
// (dynamic-features.md § Platform-family surface excision): a store-safe build has
// no PaymentsProvidersListAsync / PaymentsClaimsListAsync to produce them and no
// §4/§5 render to consume them, so shipping the types would be dead API on a plane
// the artifact is supposed not to have. ─────────────────────────────────────────
/// <summary>
/// One of the author's own configured payment providers, projected for the
/// profile Tiers tab §4 (<c>subscription-provider-list</c> rows —
/// <c>monetization.md</c> § Pillar 3, IDs reserved 2026-07-12). Rows never
/// carry the webhook secret — the nest deliberately omits it from
/// <c>providers.list</c>, so editing means re-entering it (linux
/// <c>build_provider_row</c>).
/// </summary>
public sealed record PaymentProviderRow(string Kind, string Tier, ulong? LastVerifiedAt, ulong? LastRejectedAt)
{
    /// <summary>ui.yaml's <c>subscription-provider-status</c> contract is
    /// <c>configured | verified | error</c> — the shared evidence-based
    /// decision (<c>fauna_core::format::provider_status_label</c>, never an
    /// active probe; FFI <c>ProviderStatusLabel</c>), the single source every
    /// app resolves (mirrors linux's <c>build_provider_row</c>). No local
    /// twin of the verified/rejected-wins branch.</summary>
    public string Status =>
        FaunaApp.Core.Services.Strings.Resolve(
            uniffi.fauna_ffi.FaunaFfiMethods.ProviderStatusLabel(LastVerifiedAt, LastRejectedAt));
}

/// <summary>
/// One claim code — manually- or webhook-minted — projected for the profile
/// Tiers tab §5 audit list (<c>subscription-claim-list</c> rows;
/// <c>monetization.md</c> § Pillar 3). <see cref="StatusLabel"/> is resolved by
/// the VM from the shared 3-state decision
/// (<c>fauna_core::format::claim_status_label</c> — redeemed wins over voided;
/// FFI <c>ClaimStatusLabel</c>), the single source every app shares.
/// </summary>
public sealed record PaymentClaimRow(string Code, string Tier, string StatusLabel);
#endif   // PAYMENTS
