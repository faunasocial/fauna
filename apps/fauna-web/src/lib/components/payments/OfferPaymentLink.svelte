<script lang="ts">
  // The offer row's external payment link, `subscription-offer-payment-link` —
  // the id whose survival in the excised iOS archive raised the price-and-route
  // ruling (dynamic-features.md § Platform-family surface excision → *The
  // price-and-route class*). Isolated here, imported only from behind
  // `__FAUNA_PAYMENTS__`. The open itself stays the page's `openPayment`, whose
  // `isSafeNavUrl` guard is glue, not a payments face.
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    onopen: () => void;
  }
  let { onopen }: Props = $props();
</script>

<button class="btn" data-testid={IDS.SUBSCRIPTION_OFFER_PAYMENT_LINK} onclick={onopen}
>{t.subscriptions.payment_url}</button>

<style>
  /* Scoped copy of the profile page's own `.btn` — see `AskingPriceInput.svelte`. */
  .btn {
    padding: 0.35rem 0.7rem;
    border-radius: 6px;
    border: 1px solid var(--border, #30363d);
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
  }
  .btn:hover { background: var(--bg-hover, #21262d); }
</style>
