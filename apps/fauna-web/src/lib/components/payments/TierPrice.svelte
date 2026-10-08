<script lang="ts">
  // A tier's price text — the tier list's `subscription-tier-price` and the
  // offer row's `subscription-offer-price` — buyer-side members of the
  // price-and-route class (dynamic-features.md § Platform-family surface
  // excision → *The price-and-route class*). Isolated here, imported only from
  // behind `__FAUNA_PAYMENTS__`, so an excised build shows a priced tier as an
  // ordinary approval-gated tier and its bundle carries neither id.
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    variant: 'tier' | 'offer';
    priceHint: string | null | undefined;
  }
  let { variant, priceHint }: Props = $props();

  const testid = $derived(
    variant === 'tier' ? IDS.SUBSCRIPTION_TIER_PRICE : IDS.SUBSCRIPTION_OFFER_PRICE
  );
</script>

<span data-testid={testid}>{priceHint ?? ''}</span>
