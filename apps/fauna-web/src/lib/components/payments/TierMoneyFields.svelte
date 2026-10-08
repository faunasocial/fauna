<script lang="ts">
  // The subscription tier editor's three money fields — price hint, asking
  // price, payment URL — the author half of the price-and-route class
  // (dynamic-features.md § Platform-family surface excision → *The
  // price-and-route class*). Lifted out of the profile page so the store-safe
  // bundle carries none of them: an id inside a folded `{#if}` may still ship
  // with the module that declares it, so the render must live here and be
  // imported only from behind `__FAUNA_PAYMENTS__`. An excised editor sends
  // neither field, and `tiers.update` reads a missing one as keep
  // (monetization.md § The asking price), so a priced tier survives an edit.
  import AskingPriceInput from '$lib/components/payments/AskingPriceInput.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    priceHint: string;
    askingPrice: string;
    paymentUrl: string;
  }
  let {
    priceHint = $bindable(),
    askingPrice = $bindable(),
    paymentUrl = $bindable(),
  }: Props = $props();
</script>

<input
  class="input" type="text" placeholder={t.subscriptions.price_hint}
  data-testid={IDS.SUBSCRIPTION_TIER_FORM_PRICE_HINT} bind:value={priceHint}
/>
<AskingPriceInput
  variant="subscription-tier-form"
  placeholder={t.subscriptions.asking_price}
  value={askingPrice}
  onvaluechange={(v) => { askingPrice = v; }}
/>
<input
  class="input" type="text" placeholder={t.subscriptions.payment_url}
  data-testid={IDS.SUBSCRIPTION_TIER_FORM_PAYMENT_URL} bind:value={paymentUrl}
/>

<style>
  /* Scoped copy of the profile page's own `.input` — see `AskingPriceInput.svelte`. */
  .input {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
  }
</style>
