<script lang="ts">
  import { classifySources } from '$lib/wasm';
  import { sourceGlyphEmoji } from '$lib/source-glyph';
  import { IDS } from '$lib/generated/uiIds';

  let { source }: { source: string } = $props();

  // The comma-separated wire `source` field → ordered, deduplicated
  // `{ id, label, glyph }` badges via the shared `fauna_feed::classify_sources`
  // (over wasm). Web keeps only the `SourceGlyph → emoji` map (`sourceGlyphEmoji`,
  // shared with the conversations rail), keyed off the precomputed `glyph` concept;
  // the label is the canonical shared `SourceKind::label`. One `protocol-badge` per
  // classified source — matching linux `build_protocol_badges` (`feed.md` § Where
  // logic lives). An empty / whitespace `source` classifies to no badge.
  let badges = $derived(classifySources(source));
</script>

{#each badges as badge}
  <span class="post-source source-{badge.id}" data-testid={IDS.PROTOCOL_BADGE} title={badge.label}>{sourceGlyphEmoji(badge.glyph)} {badge.label}</span>
{/each}

<style>
  .post-source {
    display: inline-flex;
    align-items: center;
    font-size: 0.7rem;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
    font-weight: 500;
    white-space: nowrap;
    border: 1px solid var(--border);
    background: var(--bg-surface);
    color: var(--text-muted);
  }
  .post-source.source-fauna { border-color: #22c55e; color: #16a34a; }
  .post-source.source-bluesky { border-color: #3b82f6; color: #2563eb; }
  .post-source.source-activitypub { border-color: #a855f7; color: #7c3aed; }
  .post-source.source-nostr { border-color: #f59e0b; color: #d97706; }
  .post-source.source-email { border-color: #6b7280; color: #4b5563; }
</style>
