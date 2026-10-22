<script lang="ts">
  // The profile Tiers tab's §5 manual-claim-code section (`monetization.md`
  // § Pillar 3), lifted out of `routes/profile/[[actorId]]/+page.svelte` for the
  // web `payments` excision leg. See `ProviderSection.svelte`'s header for why
  // the read logic had to travel with the markup rather than stay on the page.
  //
  // Mint covers the no-API providers (bank transfer, cash) the creator settles
  // out-of-band; the list is the audit surface for BOTH manually- and
  // webhook-minted codes — for the latter, the webhook HTTP response body is the
  // only other delivery channel. Void withdraws one of the author's own codes
  // (`claims.void`): the first press on a row arms it in place, the second press
  // on the same button confirms.
  import { t } from '#lib/i18n/strings';
  import {
    paymentsClaimsMint,
    paymentsClaimsList,
    paymentsClaimsVoid,
    claimStatusLabel,
    type PaymentClaim,
  } from '#lib/payments';
  import { PAYMENTS_IDS } from '#lib/components/payments/generatedIds';

  interface Props {
    /** The logged-in actor's secret, or null before identity resolves. */
    secretHex: string | null;
    /** The author's own §1 tiers — a claim entitles exactly one of them. */
    tiers: { name: string }[];
    /** Bumped by the page wherever it reloads the SELF surfaces. */
    reloadTick: number;
    /** Surface a failure on the page's shared `error-message` element. */
    onerror: (message: string) => void;
  }
  let { secretHex, tiers, reloadTick, onerror }: Props = $props();

  let claims = $state<PaymentClaim[]>([]);
  let claimTier = $state('');
  // The claim CODE whose Void is armed — keyed by code, not row index, so a
  // re-read that reorders or removes rows cannot leave the wrong row armed.
  let armedCode = $state<string | null>(null);

  $effect(() => {
    const s = secretHex;
    void reloadTick;
    if (!s) return;
    void load(s);
  });

  // Keep the picker on a tier that still exists, preserving the current choice
  // by name — the page did this inside `loadSelf` before the lift.
  $effect(() => {
    const names = tiers.map((tr) => tr.name);
    if (names.length > 0 && !names.includes(claimTier)) claimTier = names[0];
  });

  async function load(s: string): Promise<void> {
    try {
      claims = await paymentsClaimsList(s);
      armedCode = null;
    } catch (e) {
      onerror(e instanceof Error ? e.message : String(e));
    }
  }

  // No client-side validation — the nest rejects a tier that isn't one of the
  // author's own with its own typed error.
  async function mint(): Promise<void> {
    const s = secretHex;
    if (!s || !claimTier) return;
    try {
      await paymentsClaimsMint(s, claimTier);
      onerror('');
      await load(s);
    } catch (e) {
      onerror(e instanceof Error ? e.message : String(e));
    }
  }

  // The consequence the second press has: a redeemed code's void also ends its
  // redeemer's entitlement to the tier.
  function voidLabel(claim: PaymentClaim): string {
    if (armedCode !== claim.code) return t.subscriptions.void_claim;
    return claim.redeemed_by !== null
      ? t.subscriptions.void_claim_confirm_redeemed({ redeemer: claim.redeemed_by, tier: claim.tier })
      : t.subscriptions.void_claim_confirm_unredeemed;
  }

  async function voidClaim(claim: PaymentClaim): Promise<void> {
    const s = secretHex;
    if (!s) return;
    if (armedCode !== claim.code) {
      armedCode = claim.code;
      return;
    }
    armedCode = null;
    try {
      await paymentsClaimsVoid(s, claim.code);
      onerror('');
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      // The row is no longer the author's to void: the list is what is stale,
      // so re-read it rather than report an error about a row that should go.
      if (!message.includes('fauna.payments.claim_not_found')) onerror(message);
    }
    await load(s);
  }
</script>

<section class="section" data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_SECTION}>
  <h2>{t.subscriptions.manual_claims}</h2>
  <div class="select-row">
    <label for="claim-tier-select">{t.subscriptions.provider_tier_label}</label>
    <select
      id="claim-tier-select" class="input"
      data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_TIER_SELECT} bind:value={claimTier}
    >
      {#each tiers as tier (tier.name)}
        <option value={tier.name}>{tier.name}</option>
      {/each}
    </select>
    <button
      class="btn primary" data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_MINT_BUTTON}
      onclick={mint} disabled={!claimTier}
    >{t.subscriptions.mint_claim}</button>
  </div>

  <div class="list" data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_LIST}>
    {#each claims as claim (claim.code)}
      <div class="row" data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_ROW}>
        <span class="grow mono" data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_CODE}>{claim.code}</span>
        <span data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_TIER}>{claim.tier}</span>
        <span class="badge" data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_STATUS}>
          {claimStatusLabel(claim.redeemed_by !== null, claim.voided_at !== null)}
        </span>
        {#if claim.voided_at === null}
          <button
            class="btn" data-testid={PAYMENTS_IDS.SUBSCRIPTION_CLAIM_VOID_BUTTON}
            onclick={() => voidClaim(claim)}
          >{voidLabel(claim)}</button>
          {#if armedCode === claim.code}
            <button class="btn" onclick={() => (armedCode = null)}>{t.subscriptions.void_claim_cancel}</button>
          {/if}
        {/if}
      </div>
    {/each}
    {#if claims.length === 0}
      <p class="muted">{t.subscriptions.no_claims}</p>
    {/if}
  </div>
</section>

<style>
  /* Scoped copies of the profile page's section chrome — see
     `ProviderSection.svelte`'s matching note. */
  .section {
    margin: 1.25rem 0;
    padding: 1rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    background: var(--bg-surface, #161b22);
  }
  .section h2 { font-size: 1rem; margin: 0 0 0.75rem; }
  .muted { color: var(--text-muted, #8b949e); }
  .mono {
    font-family: monospace;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grow { flex: 1; }
  .badge {
    font-size: 0.75rem;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    background: var(--bg-hover, #21262d);
    color: var(--text-muted, #8b949e);
  }
  .list { display: flex; flex-direction: column; gap: 0.5rem; margin-top: 0.75rem; }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
  }
  .select-row { display: flex; align-items: center; gap: 0.5rem; }
  .input {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
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
  .btn:disabled { opacity: 0.6; cursor: default; }
  .btn.primary { border-color: var(--accent, #58a6ff); color: var(--accent, #58a6ff); }
</style>
