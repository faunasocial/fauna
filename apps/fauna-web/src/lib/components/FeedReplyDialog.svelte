<script lang="ts">
  // The feed card's reply compose dialog (`feed-reply-dialog`) over a
  // `FeedPostHost` — reply is COMPOSED via FeedManager::reply, never
  // `interact`, whose native arm discards `body` (feed.md § Implementation
  // status today). Painted by whichever page painted the card — the Feed page,
  // or the profile's Posts tab in place (`profile.md` § The Posts tab) — so the
  // reply lands through the manager the row came from. Prior art: linux's
  // build_reply_dialog; no cancel beyond the button, click-outside and Escape.
  import { contactsRevision } from '#lib/conversations';
  import { authorName, type FeedPostHost } from '#lib/feed-post-host.svelte';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let { host }: { host: FeedPostHost } = $props();

  const target = $derived(host.replyTarget());
  const targetAuthor = $derived.by<string>(() => {
    void $contactsRevision;
    return target ? authorName(target) : '';
  });
  const audienceCopy = $derived.by<string>(() => {
    const p = target;
    switch (p?.reply_audience) {
      case 'SealedToRoom':
        return t.feed.post.reply_audience_room({ room: p.room_label ?? '' });
      case 'SealedToTier':
        return t.feed.post.reply_audience_tier({ tier: p.gated_tier ?? '' });
      case 'PublicByConfirmation':
        return t.feed.post.reply_audience_public;
      default:
        return '';
    }
  });
</script>

{#if host.reply.targetId}
  <div
    class="compose-overlay"
    role="dialog"
    aria-modal="true"
    aria-label={t.common.reply}
    tabindex="-1"
    onclick={(e) => { if (e.target === e.currentTarget) host.closeReplyDialog(); }}
    onkeydown={(e) => { if (e.key === 'Escape') host.closeReplyDialog(); }}
  >
    <div class="compose-dialog" data-testid={IDS.FEED_REPLY_DIALOG}>
      <h3>{t.feed.post.replying_to_user({ user: targetAuthor })}</h3>
      {#if audienceCopy}
        <p class="compose-dialog-audience" data-testid={IDS.FEED_REPLY_AUDIENCE}>{audienceCopy}</p>
      {/if}
      <textarea
        class="compose-dialog-textarea"
        data-testid={IDS.FEED_REPLY_TEXT_FIELD}
        placeholder={t.feed.post.write_reply}
        rows="4"
        bind:value={host.reply.text}
      ></textarea>
      {#if target?.reply_audience === 'PublicByConfirmation'}
        <label class="compose-dialog-public-confirm">
          <input
            type="checkbox"
            data-testid={IDS.FEED_REPLY_PUBLIC_CONFIRM}
            data-checked={host.reply.publicConfirm ? 'true' : 'false'}
            bind:checked={host.reply.publicConfirm}
          />
          {t.feed.post.reply_public_confirm}
        </label>
      {/if}
      <div class="compose-dialog-actions">
        <button class="btn-secondary" onclick={host.closeReplyDialog}>{t.common.cancel}</button>
        <button
          class="btn-primary"
          data-testid={IDS.FEED_REPLY_SUBMIT_BUTTON}
          disabled={!host.reply.text.trim()}
          onclick={host.submitReply}
        >{t.common.reply}</button>
      </div>
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
  .compose-dialog-textarea {
    width: 100%;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.5rem;
    font-size: 0.9rem;
    resize: vertical;
    color: var(--text);
    font-family: inherit;
    box-sizing: border-box;
  }
  .compose-dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .btn-primary {
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: 4px;
    padding: 0.35rem 0.8rem;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .btn-primary:disabled {
    opacity: 0.5;
    cursor: default;
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
</style>
