<script lang="ts">
  import { IDS } from '#lib/generated/uiIds';
  import { classifySources } from '#lib/wasm';

  /** The per-emoji reaction pills under a post's interaction bar
   *  (`feed.md` § Layout & flow → *Reaction posts count and paint*, ruling 4;
   *  the feed twin of `dm-reaction-pill`). One `post-reaction-pill` per group of
   *  the shared snapshot's `PostSummary.reactions`, in its order: emoji + count,
   *  the viewer's own (`viewer_react_id` present) highlighted and carrying
   *  `data-state` `on`/`off`. A tap fires the manager's `react` toggle through
   *  `onreact`. A `bluesky` row paints none (ruling 5: its counters live at the
   *  origin) — classified by the shared `classifySources`, never a page-side
   *  string compare; the caller keeps a repost row out. */
  interface Group {
    emoji: string;
    count: number;
    viewer_react_id?: string | null;
  }
  interface Props {
    post_id: string;
    source?: string;
    reactions?: Group[];
    onreact: (post_id: string, emoji: string) => void;
  }

  let { post_id, source = '', reactions = [], onreact }: Props = $props();

  let groups = $derived(
    classifySources(source, []).some((b) => b.id === 'bluesky')
      ? []
      : reactions.filter((g) => g.count > 0),
  );
</script>

{#if groups.length > 0}
  <div class="reaction-pills">
    {#each groups as g (g.emoji)}
      {@const mine = !!g.viewer_react_id}
      <button
        class="reaction-pill"
        class:mine
        aria-pressed={mine}
        data-testid={IDS.POST_REACTION_PILL}
        data-state={mine ? 'on' : 'off'}
        title={`${g.emoji} ${g.count}`}
        onclick={() => onreact(post_id, g.emoji)}
      >{g.emoji} {g.count}</button>
    {/each}
  </div>
{/if}

<style>
  .reaction-pills { display: flex; flex-wrap: wrap; gap: 0.25rem; margin-top: 0.35rem; }
  .reaction-pill {
    border: 1px solid var(--border); border-radius: 999px; background: var(--bg-surface);
    color: var(--text); cursor: pointer; font-size: 0.75rem; padding: 0.1rem 0.5rem;
  }
  .reaction-pill:hover { background: var(--bg-hover); }
  .reaction-pill.mine { background: var(--accent); color: #fff; border-color: var(--accent); }
</style>
