<script lang="ts">
  // A bought sold post's `gated-post-purchase-pending` line — the buyer's
  // "bought, the author's grant still on its way" state that replaces the buy
  // button (monetization.md § Per-post pay-to-unlock → *a completed buy must
  // not be re-offered*), a member of the price-and-route class
  // (dynamic-features.md § Platform-family surface excision). Imported only from
  // behind `__FAUNA_PAYMENTS__`, like `SoldPostTeaser.svelte`, so an excised
  // build's bundle carries no trace of the id.
  import { t } from '#lib/i18n/strings';
  import { PAYMENTS_IDS } from '#lib/components/payments/generatedIds';
</script>

<span class="muted" data-testid={PAYMENTS_IDS.GATED_POST_PURCHASE_PENDING}>{t.feed.post.purchase_pending}</span>

<style>
  /* Scoped copy of `PostCard.svelte`'s own `.muted`. */
  .muted { color: var(--text-muted); }
</style>
