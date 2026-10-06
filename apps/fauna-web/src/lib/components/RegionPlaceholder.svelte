<script lang="ts">
  // The placeholder a REGION verdict paints in place of the withheld item
  // (`region-blocking.md` § The blocked render and the transparency surface):
  // the app's frame naming the region and its authority, the authority's name,
  // and its reason verbatim; a `collapse` adds the reveal. One component for the
  // feed card, the post detail and the conversation bubble — linux
  // `region::placeholder_box`, tui `region::placeholder`.
  //
  // `data-verdict` carries the verb so the convention-17 walk
  // (`regionBlockRenderForTest`) counts painted BLOCK placeholders — tui's
  // `VERDICT_ATTR`.
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';
  import type { RegionPlaceholder } from '$lib/wasm';

  let {
    placeholder,
    onreveal,
    class: klass = 'content-policy-collapse',
  }: {
    placeholder: RegionPlaceholder;
    onreveal?: () => void;
    class?: string;
  } = $props();

  let notice = $derived(
    placeholder.verb === 'block'
      ? t.region.blocked_notice({ region: placeholder.region, authority: placeholder.authorityName })
      : t.region.collapsed_notice({ region: placeholder.region, authority: placeholder.authorityName }),
  );
</script>

<div class="region-placeholder {klass}">
  <span class="muted" data-testid={IDS.REGION_BLOCKED_NOTICE} data-verdict={placeholder.verb}>{notice}</span>
  <span class="muted small" data-testid={IDS.REGION_BLOCKED_AUTHORITY}>{placeholder.authorityName}</span>
  <span class="muted small" data-testid={IDS.REGION_BLOCKED_REASON}>{placeholder.reason}</span>
  {#if placeholder.verb === 'collapse'}
    <button
      class="btn-secondary small"
      data-testid={IDS.REGION_COLLAPSED_REVEAL_BUTTON}
      onclick={(e) => { e.stopPropagation(); onreveal?.(); }}
    >{t.region.reveal_button}</button>
  {/if}
</div>

<style>
  .region-placeholder {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    align-items: flex-start;
  }
</style>
