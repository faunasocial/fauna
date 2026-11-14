<script lang="ts">
  // The buyer-side claim redemption (monetization.md § Pillar 3 Q4 — the
  // universal fallback binding): paste a post-payment claim code → the
  // entitlement binds to this actor and lands as a queued grant in the
  // subscriptions list above (a "pending" row, exactly like a queued subscribe).
  // Mirrors linux `settings/subscriptions.rs::redeem_claim`.
  //
  // Lifted out of `SubscriptionsSection.svelte` for the web `payments` excision
  // leg. The section around it is Pillar 1 (the user's own subscriptions) and
  // stays in every flavor; only this input is a money gesture, so only this
  // piece excises — matching tui's and linux's split exactly.
  import { t } from '#lib/i18n/strings';
  //
  // Two doors, one call shape (`monetization.md` § Pillar 3 → *Which nest an
  // in-app redeem binds on* → *The doors*): the settings pair names no home
  // (the code redeems on this nest, the author slot unread), the OTHER profile
  // Tiers tab's names the viewed creator and the page's actor home — the same
  // home the follow carries. Lifts tui's `Action::RedeemClaim` +
  // `ConsumerOp::RedeemClaim`.
  import { paymentsClaimsRedeemAcrossNests, type PaymentClaimRedeemed } from '#lib/payments';
  import { redeemClaimCode } from '#lib/components/payments/claim-redeem';
  import { PAYMENTS_IDS } from '#lib/components/payments/generatedIds';

  interface Props {
    /** The logged-in actor's secret, or null before identity resolves. */
    secretHex: string | null;
    /** The creator whose code this door redeems — the viewed actor on a
     *  profile; on the settings page (no creator in hand) the redeemer's own
     *  id, which a null `homeNestUrl` never reads. */
    authorIdHex: string;
    /** That creator's home nest, or null for this nest. */
    homeNestUrl: string | null;
    /** Re-read the host page — a redeemed claim lands there. */
    onredeemed: (reply: PaymentClaimRedeemed) => Promise<void> | void;
    /** Surface a failure on the host page's single `error-message`. */
    onerror: (message: string) => void;
  }
  let { secretHex, authorIdHex, homeNestUrl, onredeemed, onerror }: Props = $props();

  let claimCode = $state('');

  async function redeem(): Promise<void> {
    try {
      const reply = await redeemClaimCode(
        claimCode,
        secretHex,
        { authorIdHex, homeNestUrl },
        paymentsClaimsRedeemAcrossNests,
      );
      if (reply === null) return;
      // Cleared only once the nest took the code — a refused paste stays to
      // correct.
      claimCode = '';
      onerror('');
      await onredeemed(reply);
    } catch (e) {
      onerror(e instanceof Error ? e.message : String(e));
    }
  }
</script>

<h2 class="claim-title">{t.subscriptions.redeem_claim_title}</h2>
<div class="row">
  <input
    class="input grow" type="text" placeholder={t.subscriptions.claim_code}
    data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_REDEEM_INPUT} bind:value={claimCode}
  />
  <button
    class="btn primary" data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_REDEEM_BUTTON}
    onclick={redeem}
  >{t.subscriptions.redeem}</button>
</div>

<style>
  /* Scoped copies of the host section's chrome — see `ProviderSection.svelte`'s
     matching note on why these are duplicated rather than shared globally. */
  .claim-title { font-size: 1rem; margin: 1rem 0 0.5rem; }
  .grow { flex: 1; }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
  }
  .btn {
    padding: 0.35rem 0.7rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
  }
  .btn:hover { background: var(--bg-hover, #21262d); }
  .btn.primary { border-color: var(--accent, #2f81f7); color: var(--accent, #2f81f7); }
  .input {
    padding: 0.35rem 0.5rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
  }
</style>
