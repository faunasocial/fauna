<script lang="ts">
  // Factor-target sheet (`feed-post-train-target-sheet`, topic-factors.md
  // § Authoring surface) over a `FeedPostHost`: shown ONLY when the host's
  // manager has no dominant trained factor, so *more/less like this* cannot
  // train in context. Picking a topic trains this post into it. Painted by
  // whichever page painted the card (the Feed page, the profile's Posts tab).
  import type { FeedPostHost } from '#lib/feed-post-host.svelte';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let { host }: { host: FeedPostHost } = $props();

  function close(): void {
    host.trainSheet.target = null;
  }
</script>

{#if host.trainSheet.target}
  <div
    class="compose-overlay"
    role="dialog"
    aria-modal="true"
    aria-label={t.feed.train_target_title}
    tabindex="-1"
    onclick={(e) => { if (e.target === e.currentTarget) close(); }}
    onkeydown={(e) => { if (e.key === 'Escape') close(); }}
  >
    <div class="compose-dialog" data-testid={IDS.FEED_POST_TRAIN_TARGET_SHEET}>
      <h3>{t.feed.train_target_title}</h3>
      {#if host.trainSheet.topics.length === 0}
        <p class="muted">{t.personalization.trained_topics_empty}</p>
      {:else}
        {#each host.trainSheet.topics as topic (topic.id)}
          <button
            class="btn-secondary"
            data-testid={IDS.FEED_POST_TRAIN_TARGET_ITEM}
            data-factor={topic.factor_key?.replace(/^topic:/, '')}
            onclick={async () => {
              const target = host.trainSheet.target;
              close();
              if (target && topic.factor_key) {
                await host.trainInto(target.post_id, target.verb, topic.factor_key);
              }
            }}
          >{topic.name}</button>
        {/each}
      {/if}
      <a href="/app/settings/personalization" class="btn-secondary" data-testid={IDS.FEED_POST_TRAIN_TARGET_NEW_BUTTON}>{t.personalization.trained_factor_create}</a>
    </div>
  </div>
{/if}

<style>
  .compose-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .compose-dialog {
    background: var(--bg-surface);
    border: 2px solid var(--border);
    border-radius: 8px;
    padding: 1.25rem;
    max-width: 600px;
    width: 90%;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .compose-dialog h3 {
    margin: 0;
    font-size: 1rem;
  }
  .btn-secondary {
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.35rem 0.8rem;
    font-size: 0.85rem;
    cursor: pointer;
    color: var(--text);
  }
  .btn-secondary:hover {
    background: var(--bg-hover);
  }
  .muted {
    color: var(--text-muted);
    font-size: 0.85rem;
  }
</style>
