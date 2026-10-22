<script lang="ts">
  // The placeholder a REGION or NEST verdict paints in place of the withheld item
  // (`region-blocking.md` § The blocked render and the transparency surface;
  // `nest-rules.md` § Element IDs): the app's frame naming the source, its name,
  // and the publisher's reason verbatim; a `collapse` adds the reveal. One
  // component for the feed card, the post detail and the conversation bubble —
  // linux `region::placeholder_box`, tui `region::placeholder`
  // / `nest_rules::placeholder`. A region's placeholder carries `region` and the
  // authority's name; a nest's carries the nest's `domain`.
  //
  // `data-verdict` carries the verb so the convention-17 walks
  // (`regionBlockRenderForTest`, `nestBlockRenderForTest`) count painted BLOCK
  // placeholders — tui's `VERDICT_ATTR`.
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';
  import type { ItemPlaceholder } from '#lib/wasm';

  let {
    placeholder,
    onreveal,
    class: klass = 'content-policy-collapse',
  }: {
    placeholder: ItemPlaceholder;
    onreveal?: () => void;
    class?: string;
  } = $props();

  let notice = $derived(
    'domain' in placeholder
      ? placeholder.verb === 'block'
        ? t.nest_rules.blocked_notice({ nest: placeholder.domain })
        : t.nest_rules.collapsed_notice({ nest: placeholder.domain })
      : placeholder.verb === 'block'
        ? t.region.blocked_notice({ region: placeholder.region, authority: placeholder.authorityName })
        : t.region.collapsed_notice({ region: placeholder.region, authority: placeholder.authorityName }),
  );
</script>

<div class="region-placeholder {klass}">
  {#if 'domain' in placeholder}
    <span class="muted" data-testid={IDS.NEST_BLOCKED_NOTICE} data-verdict={placeholder.verb}>{notice}</span>
    <span class="muted small" data-testid={IDS.NEST_BLOCKED_DOMAIN}>{placeholder.domain}</span>
    <span class="muted small" data-testid={IDS.NEST_BLOCKED_REASON}>{placeholder.reason}</span>
    {#if placeholder.verb === 'collapse'}
      <button
        class="btn-secondary small"
        data-testid={IDS.NEST_COLLAPSED_REVEAL_BUTTON}
        onclick={(e) => { e.stopPropagation(); onreveal?.(); }}
      >{t.nest_rules.reveal_button}</button>
    {/if}
  {:else}
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
