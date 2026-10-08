<script lang="ts">
  // The "Sell this post…" controls (monetization.md § Per-post pay-to-unlock;
  // IDs user-approved 2026-07-29) — `compose-sell-price`,
  // `compose-sell-asking-price`, `compose-sell-subscribers-free` — the sell
  // composer whole, inside the `payments` plane by the price-and-route ruling
  // (dynamic-features.md § Platform-family surface excision → *The
  // price-and-route class*). Lifted out of `FeedComposeBar.svelte` and imported
  // only from behind `__FAUNA_PAYMENTS__`, which also drops the select's sell
  // answer, so an excised build can author no sale and its bundle carries none
  // of the three ids. The rank knob defaults CHECKED — an existing paying
  // subscriber isn't charged twice for a post their subscription would cover;
  // pay-per-view is the deliberate opt-in.
  import AskingPriceInput from '$lib/components/payments/AskingPriceInput.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    sellPrice: string;
    // The machine-comparable price (monetization.md § The asking price) —
    // independent of sellPrice (the free-text hint); no parsing ever infers one
    // from the other.
    sellAskingPrice: string;
    sellSubscribersFree: boolean;
    onsellpricechange?: (v: string) => void;
    onsellaskingpricechange?: (v: string) => void;
    onsellsubscribersfreechange?: (v: boolean) => void;
  }
  let {
    sellPrice, sellAskingPrice, sellSubscribersFree,
    onsellpricechange, onsellaskingpricechange, onsellsubscribersfreechange,
  }: Props = $props();
</script>

<div class="compose-meta">
  <input
    class="text-input small"
    data-testid={IDS.COMPOSE_SELL_PRICE}
    type="text"
    placeholder={t.feed.post.sell_price_placeholder}
    value={sellPrice}
    oninput={(e) => onsellpricechange?.(e.currentTarget.value)}
  />
  <AskingPriceInput
    variant="compose-sell"
    placeholder={t.feed.post.sell_asking_price_placeholder}
    value={sellAskingPrice}
    onvaluechange={(v) => onsellaskingpricechange?.(v)}
  />
  <label class="sell-subscribers-free-label">
    <input
      type="checkbox"
      data-testid={IDS.COMPOSE_SELL_SUBSCRIBERS_FREE}
      checked={sellSubscribersFree}
      data-checked={sellSubscribersFree ? 'true' : 'false'}
      onchange={(e) => onsellsubscribersfreechange?.(e.currentTarget.checked)}
    />
    {t.feed.post.sell_subscribers_free}
  </label>
</div>

<style>
  /* Scoped copies of `FeedComposeBar.svelte`'s own chrome — see
     `AskingPriceInput.svelte`. */
  .compose-meta {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }
  .sell-subscribers-free-label {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.8rem;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .text-input.small {
    flex: 1;
    font-size: 0.8rem;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--text);
  }
</style>
