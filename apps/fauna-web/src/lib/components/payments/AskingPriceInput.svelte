<script lang="ts">
  // The buyer-facing "asking price" field on a paid post's sell row
  // (FeedComposeBar) and a subscription tier's form (profile page) —
  // monetization's optional named-price alternative to a flat sell price.
  // Lifted out of both shared, ungated files for the web `payments` excision
  // leg (dynamic-features.md § Platform-family surface excision: "a gated
  // plane's user-facing INPUTS excise with it") — mirrors `ClaimRedeem.svelte`'s
  // extraction out of `SubscriptionsSection.svelte`.
  import { PAYMENTS_IDS } from '$lib/components/payments/generatedIds';

  interface Props {
    /** Which caller is rendering this — selects the id + the scoped chrome
     * matching that caller's own original inline styling exactly. */
    variant: 'compose-sell' | 'subscription-tier-form';
    value: string;
    onvaluechange: (v: string) => void;
    placeholder: string;
  }
  let { variant, value, onvaluechange, placeholder }: Props = $props();

  const testid = $derived(
    variant === 'compose-sell'
      ? PAYMENTS_IDS.COMPOSE_SELL_ASKING_PRICE
      : PAYMENTS_IDS.SUBSCRIPTION_TIER_FORM_ASKING_PRICE
  );
</script>

{#if variant === 'compose-sell'}
  <input
    class="text-input small"
    data-testid={testid}
    type="text"
    {placeholder}
    {value}
    oninput={(e) => onvaluechange(e.currentTarget.value)}
  />
{:else}
  <input
    class="input"
    type="text"
    {placeholder}
    data-testid={testid}
    {value}
    oninput={(e) => onvaluechange(e.currentTarget.value)}
  />
{/if}

<style>
  /* Scoped copies of each host's own chrome — see `ProviderSection.svelte`'s
     matching note on why these are duplicated rather than shared globally.
     `.text-input.small` mirrors FeedComposeBar.svelte; `.input` mirrors
     +page.svelte (profile)'s subscription-tier-form fields. */
  .text-input.small {
    flex: 1;
    font-size: 0.8rem;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--text);
  }
  .input {
    padding: 0.375rem 0.5rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg, #0d1117);
    color: var(--text, #e6edf3);
    font-size: 0.875rem;
  }
</style>
