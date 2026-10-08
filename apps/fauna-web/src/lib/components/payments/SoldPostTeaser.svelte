<script lang="ts">
  // A sold post's buyer surface on its card — `gated-post-price`,
  // `gated-post-payment-link`, `gated-post-buy-button` — the buyer half of the
  // price-and-route class (dynamic-features.md § Platform-family surface
  // excision → *The price-and-route class*). Lifted out of `PostCard.svelte`
  // and imported only from behind `__FAUNA_PAYMENTS__`, so an excised build
  // shows a sold post as an ordinary gated post — badge and nothing more — and
  // its bundle carries none of the three ids. The buy (`fauna.subscriptions.
  // subscribe`) and the link's `isSafeNavUrl` guard stay with the feed page:
  // glue, not payments faces.
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    /** The post's resolved `unlock_offer` (monetization.md § Per-post
     *  pay-to-unlock → the buyer's price read is post-addressed). */
    offer: { price_hint?: string | null; payment_url?: string | null };
    onopenpaymentlink?: (url: string) => void;
    onbuy?: () => void;
  }
  let { offer, onopenpaymentlink, onbuy }: Props = $props();
</script>

<span class="muted" data-testid={IDS.GATED_POST_PRICE}>{offer.price_hint ?? ''}</span>
{#if offer.payment_url}
  <button
    class="btn-secondary small"
    data-testid={IDS.GATED_POST_PAYMENT_LINK}
    onclick={(e) => { e.stopPropagation(); onopenpaymentlink?.(offer.payment_url!); }}
  >{t.subscriptions.payment_url}</button>
{/if}
<button
  class="btn-primary small"
  data-testid={IDS.GATED_POST_BUY_BUTTON}
  onclick={(e) => { e.stopPropagation(); onbuy?.(); }}
>{t.feed.post.buy_button}</button>

<style>
  /* Scoped copy of `PostCard.svelte`'s own `.muted`. */
  .muted { color: var(--text-muted); }
</style>
