<script lang="ts">
  // The post tip surface (`monetization.md` § Tips) — one component for BOTH
  // the feed list card and the post-detail pane, which carried near-identical
  // copies of this markup until the web `payments` excision leg (priority #4:
  // resolve drift, don't replicate it on a second surface).
  //
  // ⚠ THIS COMPONENT MUST STAY BEHIND `__FAUNA_PAYMENTS__`, AND THE REASON IS
  // NOT THE OBVIOUS ONE. `PostSummary.tips` is a deliberately UNGATED inert
  // record (`dynamic-features.md` § Platform-family surface excision): in an
  // excised build the resolver never populates it, so `tips` is `null` forever
  // and this markup simply paints nothing — while still shipping every
  // `post-tip-*` id in the bundle. Dead is not absent, and criterion 1 is a
  // `strings`-grep. Both shipped Rust shells (tui 2026-08-13, linux 2026-08-14)
  // were bitten in exactly this place. So the gate lives on the RENDER, at the
  // importing branch, not on the data feeding it.
  import { t } from '$lib/i18n/strings';
  import { tipAmount, tipCount, tipMore } from '$lib/payments';
  import { PAYMENTS_IDS } from '$lib/components/payments/generatedIds';

  interface Props {
    /** The post's `tips` record — the shared `fauna_feed` tip summary, resolved
     *  lazily off `fauna.tips.list` by the feed page's `augmentPost`. */
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    tips: any;
  }
  let { tips }: Props = $props();

  /** The attribution window's open state — per instance, local, never persisted:
   *  the list is an audit affordance, not a preference. */
  let listOpen = $state(false);
</script>

<!-- Absent covers both "not yet resolved" and "the nest answered untipped" —
     one empty surface for both. The two counters are guarded independently:
     `tip_count` counts every tip, `total_msats` sums only those whose receipt
     reported an amount, so a post whose every receipt carried an unparseable
     invoice shows the count and no total (rendering "0 sats" there would say
     nobody paid). -->
{#if tips && tips.tip_count > 0}
  <div class="post-tips">
    {#if tips.total_msats !== 0}
      <span class="muted" data-testid={PAYMENTS_IDS.POST_TIP_TOTAL}>{tipAmount(tips.total_msats)}</span>
    {/if}
    <span class="muted" data-testid={PAYMENTS_IDS.POST_TIP_COUNT}>{tipCount(tips.tip_count)}</span>
    <button
      class="btn-secondary small"
      data-testid={PAYMENTS_IDS.POST_TIP_LIST_BUTTON}
      onclick={(e) => { e.stopPropagation(); listOpen = !listOpen; }}
    >{t.tips.list_open}</button>
  </div>
  {#if listOpen}
    <!-- Inline, matching the `feed-post-actions-menu` shape: every id attaches
         to a real element, absent from the DOM while closed. -->
    <div class="tip-list" data-testid={PAYMENTS_IDS.POST_TIP_LIST}>
      <div class="tip-list-title">
        {#if tips.has_more}
          {t.tips.list_title} — {tipMore(tips.tip_count - tips.senders.length)}
        {:else}
          {t.tips.list_title}
        {/if}
      </div>
      <!-- Every row the nest sent, unfiltered — authenticity is settled at
           ingest and never at read (`monetization.md` § Zap receipts). -->
      {#each tips.senders as tip}
        <div class="tip-item" data-testid={PAYMENTS_IDS.POST_TIP_ITEM}>
          {tip.sender ?? tip.sender_ref ?? t.tips.sender_unknown}
          — {tip.amount_msats != null ? tipAmount(tip.amount_msats) : t.tips.amount_unknown}
        </div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  /* Scoped copies of what the two host surfaces styled. `.muted` and
     `.btn-secondary`/`.small` came from the hosts' own scoped blocks, and they
     DISAGREED: the detail pane styled the list button, the list card left
     `.btn-secondary` undefined so its button rendered as a bare default. Taking
     the richer of the two (priority #4) makes both surfaces match. */
  .muted { color: var(--text-muted); }
  .btn-secondary {
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.35rem 0.8rem;
    font-size: 0.85rem;
    cursor: pointer;
    color: var(--text);
    transition: background 0.12s;
  }
  .btn-secondary:hover { background: var(--bg-hover); }
  .btn-secondary.small {
    padding: 0.25rem 0.5rem;
    font-size: 0.8rem;
  }
  .post-tips {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.25rem;
    font-size: 0.8125rem;
  }
  .tip-list {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.125rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    padding: 0.5rem;
    margin: 0.25rem 0;
    font-size: 0.8125rem;
  }
  .tip-list-title { font-weight: 600; margin-bottom: 0.25rem; }
  .tip-item { padding: 0.125rem 0; }
</style>
