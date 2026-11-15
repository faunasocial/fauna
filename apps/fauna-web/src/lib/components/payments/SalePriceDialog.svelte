<script lang="ts">
  // A sold post's *Change price…* dialog — `feed-post-sale-price-dialog` with its
  // two price fields, save and cancel (monetization.md § The asking price →
  // *Re-pricing a sold post*; reference painter tui's `sale_price_verbs`).
  // Opens in place inside the post's ⋯ menu. The selling tier is read through
  // `tiers.list` when the dialog mounts, and the fields are pre-filled from it
  // by the shared `salePricePrefill`; the save is the shared plan over the
  // stored fields (`subscriptionsSaveSalePrice`): unchanged / saved close the
  // dialog, a refusal paints its catalog text on the page's `error-message`
  // and leaves the dialog open with what the author typed. Imported only from
  // behind `__FAUNA_PAYMENTS__`, so an excised build carries none of the ids.
  import { t } from '#lib/i18n/strings';
  import { PAYMENTS_IDS } from '#lib/components/payments/generatedIds';
  import { identity } from '#lib/store';
  import { subscriptionsSaveSalePrice, subscriptionsTiersList, type SubscriptionTier } from '#lib/rpc';
  import { salePricePrefill } from '#lib/wasm';
  import { onDestroy } from 'svelte';

  interface Props {
    /** The post's selling tier name (`gated_tier`). */
    tierName: string;
    /** The post is advertised on Nostr (`nostr_advertised_at` set): the dialog
     *  paints `sale_price.advertised_note`. */
    advertised?: boolean;
    /** The page's `error-message` line; `''` clears it. */
    onerror: (message: string) => void;
    /** Close the dialog (saved, unchanged or cancelled). */
    onclose: () => void;
    /** A price was saved (the nest may have replaced or withdrawn the post's
     *  Nostr advertisement): the page re-reads the feed. */
    onsaved?: () => void;
  }
  let { tierName, advertised = false, onerror, onclose, onsaved }: Props = $props();

  // The note's text; read at run time (a public text the catalog may lack).
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const advertisedNote: string | null = (() => { const n = (t as any).sale_price?.advertised_note; return typeof n === 'string' ? n : null; })();

  let tier = $state<SubscriptionTier | null>(null);
  let hint = $state('');
  let asking = $state('');
  let saving = $state(false);
  let open = $state(false);

  function errMessage(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  /** A catalog key (`feed.post.…`) as its text, falling back to the key. */
  function catalogText(key: string): string {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    let node: any = t;
    for (const part of key.split('.')) node = node?.[part];
    return typeof node === 'string' ? node : key;
  }

  // The verb opens the dialog and starts the tier read.
  function openDialog(e: MouseEvent): void {
    e.stopPropagation();
    open = true;
    load();
  }

  let live = true;
  onDestroy(() => { live = false; });

  function load(): void {
    const secretHex = $identity?.secretHex ?? '';
    subscriptionsTiersList(secretHex)
      .then((tiers) => {
        if (!live) return;
        const found = tiers.find((x) => x.name === tierName);
        if (!found) throw new Error(tierName);
        const prefill = salePricePrefill(found);
        hint = prefill.priceHint;
        asking = prefill.askingPrice;
        tier = found;
        onerror('');
      })
      .catch((e) => {
        if (!live) return;
        onerror(t.sale_price.error_load({ message: errMessage(e) }));
        onclose();
      });
  }

  async function save(e: MouseEvent): Promise<void> {
    e.stopPropagation();
    if (!tier || saving) return;
    saving = true;
    try {
      const res = await subscriptionsSaveSalePrice($identity?.secretHex ?? '', tier, hint, asking);
      if (res.outcome === 'refused') {
        onerror(catalogText(res.i18nKey));
        return;
      }
      onerror('');
      onclose();
      onsaved?.();
    } catch (err) {
      onerror(t.sale_price.error_save({ message: errMessage(err) }));
    } finally {
      saving = false;
    }
  }

  function cancel(e: MouseEvent): void {
    e.stopPropagation();
    onclose();
  }
</script>

{#if !open}
  <button
    class="menu-item"
    data-testid={PAYMENTS_IDS.FEED_POST_SALE_PRICE_BUTTON}
    onclick={openDialog}
  >{t.sale_price.button}</button>
{:else if tier}
  <div class="menu-confirm" data-testid={PAYMENTS_IDS.FEED_POST_SALE_PRICE_DIALOG} role="group">
    <p class="menu-note"><strong>{t.sale_price.title}</strong></p>
    {#if advertised && advertisedNote}
      <p class="menu-note" data-testid={PAYMENTS_IDS.FEED_POST_SALE_PRICE_ADVERTISED_NOTE}>{advertisedNote}</p>
    {/if}
    <input
      class="text-input"
      type="text"
      data-testid={PAYMENTS_IDS.FEED_POST_SALE_PRICE_HINT}
      placeholder={t.feed.post.sell_price_placeholder}
      bind:value={hint}
      onclick={(e) => e.stopPropagation()}
    />
    <input
      class="text-input"
      type="text"
      data-testid={PAYMENTS_IDS.FEED_POST_SALE_PRICE_ASKING}
      placeholder={t.feed.post.sell_asking_price_placeholder}
      bind:value={asking}
      onclick={(e) => e.stopPropagation()}
    />
    <button
      class="menu-item"
      data-testid={PAYMENTS_IDS.FEED_POST_SALE_PRICE_SAVE_BUTTON}
      disabled={saving}
      onclick={save}
    >{t.common.save}</button>
    <button
      class="menu-item"
      data-testid={PAYMENTS_IDS.FEED_POST_SALE_PRICE_CANCEL_BUTTON}
      onclick={cancel}
    >{t.common.cancel}</button>
  </div>
{/if}

<style>
  .menu-confirm { display: flex; flex-direction: column; gap: 0.35rem; padding: 0.4rem 0.6rem; }
  .menu-note { margin: 0; font-size: 0.8rem; color: var(--text-muted); }
  .text-input {
    font-size: 0.8rem;
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--text);
  }
  .menu-item {
    text-align: left;
    padding: 0.35rem 0.6rem;
    background: none;
    border: 0;
    color: var(--text);
    font-size: 0.85rem;
    cursor: pointer;
  }
</style>
