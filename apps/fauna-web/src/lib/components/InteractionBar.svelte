<script lang="ts">
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  /** The feed interaction bar — like / reply / repost / quote, rendered
   *  **icon + count** with the count hidden at 0 (ratified 2026-06-27,
   *  `feed.md` § Interaction bar; uniform across all 7 apps). The four counts
   *  are read straight from the shared snapshot `PostSummary.{like,reply,repost,
   *  quote}_count` (threaded in by `PostCard`), never computed per client. The
   *  glyphs are web's idiomatic asset for the shared interaction concepts
   *  (like / reply / repost / quote) — apple renders the same concepts as SF
   *  Symbols (`heart` / `bubble.right` / `arrow.2.squarepath`); each app keeps
   *  its own asset. The i18n action strings stay as the accessible name + tooltip
   *  now that the word labels are gone. */
  interface Props {
    post_id: string;
    like_count?: number;
    reply_count?: number;
    repost_count?: number;
    quote_count?: number;
    /** The like TOGGLE's lit state, off the shared snapshot's `viewer_liked`
     *  (`feed.md` § Interaction bar → Repost ratifies the carrier) — web's
     *  vocabulary for tui's `state=on/off` attr. The next tap reverses a lit
     *  ♥, so the state has to be visible; `aria-pressed` makes the button the
     *  toggle it now is rather than an action that only ever fires once. */
    viewer_liked?: boolean;
    /** The repost TOGGLE's lit state, off the shared snapshot's
     *  `viewer_repost_id` (feed.md § Interaction bar → Repost, ratified
     *  2026-08-10) — same idiom as `viewer_liked` above. */
    viewer_reposted?: boolean;
    oninteract: (post_id: string, action: string) => void;
  }

  let {
    post_id,
    like_count = 0,
    reply_count = 0,
    repost_count = 0,
    quote_count = 0,
    viewer_liked = false,
    viewer_reposted = false,
    oninteract,
  }: Props = $props();
</script>

<div class="post-actions">
  <button class="action-btn" class:liked={viewer_liked} aria-pressed={viewer_liked} data-testid={IDS.FEED_LIKE_BUTTON} title={t.feed.like_tooltip} aria-label={t.feed.like_tooltip} onclick={() => oninteract(post_id, 'like')}>
    <span class="icon" aria-hidden="true">♥</span>{#if like_count > 0}<span class="count">{like_count}</span>{/if}
  </button>
  <button class="action-btn" data-testid={IDS.FEED_REPLY_BUTTON} title={t.common.reply} aria-label={t.common.reply} onclick={() => oninteract(post_id, 'reply')}>
    <span class="icon" aria-hidden="true">↩</span>{#if reply_count > 0}<span class="count">{reply_count}</span>{/if}
  </button>
  <button class="action-btn" class:reposted={viewer_reposted} aria-pressed={viewer_reposted} data-testid={IDS.FEED_REPOST_BUTTON} title={t.feed.post.repost} aria-label={t.feed.post.repost} onclick={() => oninteract(post_id, 'repost')}>
    <span class="icon" aria-hidden="true">⇄</span>{#if repost_count > 0}<span class="count">{repost_count}</span>{/if}
  </button>
  <button class="action-btn" data-testid={IDS.FEED_QUOTE_BUTTON} title={t.feed.quote} aria-label={t.feed.quote} onclick={() => oninteract(post_id, 'quote')}>
    <span class="icon" aria-hidden="true">❝</span>{#if quote_count > 0}<span class="count">{quote_count}</span>{/if}
  </button>
</div>

<style>
  .post-actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.5rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }
  .action-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.2rem 0.6rem;
    cursor: pointer;
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .action-btn:hover { background: var(--bg-hover); color: var(--text); }
  /* The lit ♥ — the viewer's own live like, which the next tap reverses. */
  .action-btn.liked { color: #f43f5e; border-color: #f43f5e; }
  /* The lit repost — the viewer's own live repost, which the next tap reverses. */
  .action-btn.reposted { color: #22c55e; border-color: #22c55e; }
  .icon {
    font-size: 0.95rem;
    line-height: 1;
  }
  .count {
    font-variant-numeric: tabular-nums;
  }
</style>
