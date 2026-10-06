using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The <c>subscription-settings</c> consumer page's view model — the canonical
/// cross-app consumer subscriptions surface (<c>monetization.md</c> § Pillar 1
/// consumer path). Mirrors apple <c>SubscriptionsSettingsVM</c> / linux
/// <c>subscription_settings.rs</c>: observer-free, a manual re-read after each
/// mutation (no client-side caching — <c>feed.md</c> § Architectural rules). The
/// consumer sees their own active and pending subscriptions and can unsubscribe
/// (plaintext drops the row immediately; encrypted queues a request until the
/// author rotates the period key).
///
/// <para>No <c>ConfigureAwait(false)</c> on any await — bound-state mutation must
/// resume on the UI thread (the WinUI VM rule).</para>
/// </summary>
public class SubscriptionsSettingsViewModel : ViewModelBase
{
    private readonly INestRpcClient _nest;

    /// <summary>The consumer's own active and pending subscriptions (the
    /// <c>subscription-mine-list</c> collection source).</summary>
    public ObservableCollection<SubscriptionMineRow> Subscriptions { get; } = new();

    internal SubscriptionsSettingsViewModel(INestRpcClient nest)
    {
        _nest = nest;
    }

    /// <summary>Re-read the consumer's own subscriptions and repopulate the
    /// collection. Runs on mount and after each mutation (observer-free).</summary>
    public async Task HydrateAsync()
    {
        ErrorMessage = null;
        try
        {
            var mine = await _nest.SubscriptionMineListAsync();
            Subscriptions.Clear();
            foreach (var s in mine)
                Subscriptions.Add(new SubscriptionMineRow(
                    s.authorId,
                    s.authorDisplay,
                    s.tier,
                    s.status,
                    s.since));
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Unsubscribe from <paramref name="authorId"/> then re-read (plaintext
    /// drops the row; encrypted leaves it pending until the author's key rotation).
    /// Same error handling as <see cref="HydrateAsync"/>.</summary>
    public async Task UnsubscribeAsync(byte[] authorId)
    {
        ErrorMessage = null;
        try
        {
            await _nest.SubscriptionUnsubscribeAsync(authorId);
            await HydrateAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

#if PAYMENTS
    // The buyer-side half of the payments plane, gated with it
    // (dynamic-features.md § Platform-family surface excision).
    /// <summary>Redeem a post-payment claim code (<c>fauna.payments.claims.redeem</c>
    /// — <c>monetization.md</c> § Pillar 3 Q4's universal fallback binding): binds
    /// the entitlement to this actor, then re-reads so the queued grant renders
    /// exactly like a queued subscribe (a "pending" row). Typed
    /// <c>fauna.payments.claim_{not_found,already_redeemed,voided}</c> errors
    /// surface via <see cref="ViewModelBase.ShowError"/>. A blank code is a
    /// no-op (mirrors linux/android's trim-then-empty-check).</summary>
    public async Task RedeemClaimAsync(string code)
    {
        var trimmed = code.Trim();
        if (trimmed.Length == 0)
            return;
        ErrorMessage = null;
        try
        {
            await _nest.PaymentsClaimsRedeemAsync(trimmed);
            await HydrateAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }
#endif   // PAYMENTS
}
