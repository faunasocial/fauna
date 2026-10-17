<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { goto } from '$app/navigation';
  import { identity, reconnectTick, familyTabGated } from '$lib/store';
  import { t } from '$lib/i18n/strings';
  import { getNotifications, markNotificationsRead, navigableDestination, notificationRowText, notificationTypeGlyph, type UnifiedNotification } from '$lib/notifications';
  import type { NotificationDestination } from '$lib/wasm';
  import { setPendingSearchNav } from '$lib/search';
  import { isSafeNavUrl } from '$lib/safe-url';
  import { onPushEvent, staleSurfacesForPushKind } from '$lib/rpc';
  import { relativeTime } from '$lib/value-format';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { IDS } from '$lib/generated/uiIds';

  let error = $state('');
  let notifications: UnifiedNotification[] = $state([]);
  let loading = $state(true);
  let unreadCount = $derived(notifications.filter(n => !n.is_read).length);

  let unsubPush: (() => void) | null = null;
  let unsubReconnect: (() => void) | null = null;

  /** The page's one fetch path. Called on mount, on a `fauna.notification` push,
   *  and on reconnect — a push is a hint, never the only path to a value
   *  (`transport.md` § Push events), so the fetch has to stay reachable from all
   *  three. Errors surface in the shared `error-message` banner. */
  async function load() {
    const id = $identity;
    if (!id) { loading = false; return; }
    try {
      const resp = await getNotifications(id.secretHex);
      notifications = resp.notifications;
    } catch (e: any) {
      error = e.message || t.common.load_failed;
    } finally {
      loading = false;
    }
  }

  onMount(async () => {
    await load();

    // Live refresh — the web twin of linux's central push dispatch
    // (`app.rs`: `PushEvent::Notification` → `fetch_notifications()`). Before
    // this, a notification arriving while the user sat on this page stayed
    // invisible until they navigated away and back: the page had no poll and no
    // push, so mount was its only fetch. Checked through the shared classifier
    // (`transport.md` § Which surfaces a push invalidates) rather than matching
    // `kind` by hand — this also makes `fauna.protocol.resync_required` refresh
    // this page, which the old exact-match check never did.
    unsubPush = onPushEvent((kind) => {
      if (!staleSurfacesForPushKind(kind).notifications) return;
      void load();
    });

    // Reconnect backstop: pushes fired while the socket was down are never
    // replayed, so a page that stayed mounted across the gap must re-pull. Linux
    // sweeps notifications on both `Reconnected` and `ResyncRequired` for exactly
    // this reason. `reconnectTick` is a `writable(0)` — it fires once on
    // subscribe, so skip that seed value (the feed page's idiom).
    let firstTick = true;
    unsubReconnect = reconnectTick.subscribe(() => {
      if (firstTick) { firstTick = false; return; }
      void load();
    });
  });

  onDestroy(() => {
    unsubPush?.();
    unsubReconnect?.();
  });

  /** Open a row's destination as a direct click on that page would — the Post
   *  arm through the same cross-page deep-link slot a search result uses (the
   *  feed's own `openPostDetail` resolves a post its window never loaded), Knock
   *  into Contacts (its People segment lists pending knocks), Family into the
   *  Family page, External in a new tab through the ordinary external-link guard
   *  (`notifications.md` § Deep-link destinations). */
  function openDestination(dest: NotificationDestination): void {
    if (dest === 'Family') {
      void goto('/app/family');
    } else if ('Post' in dest) {
      setPendingSearchNav({ kind: 'post', postId: dest.Post.post_id });
      void goto('/app/feed');
    } else if ('Knock' in dest) {
      void goto('/app/contacts');
    } else if (isSafeNavUrl(dest.External.url)) {
      window.open(dest.External.url, '_blank', 'noopener,noreferrer');
    }
  }

  async function markAllRead() {
    const id = $identity;
    if (!id) return;
    try {
      await markNotificationsRead(id.secretHex);
      notifications = notifications.map(n => ({ ...n, is_read: true }));
    } catch (e: any) {
      error = e.message;
    }
  }

</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.common.notifications}</h1>

<MessageBanner bind:error />

{#if !$identity}
  <p class="muted">{t.common.sign_in_required}</p>
{:else if loading}
  <p class="muted">{t.common.loading}</p>
{:else}
  <div class="notifications-header">
    <span data-testid={IDS.NOTIFICATION_COUNT_BADGE} class="badge">{unreadCount} unread</span>
    <button data-testid={IDS.NOTIFICATION_MARK_READ} class="btn" onclick={markAllRead}>{t.bridges.mark_all_read}</button>
  </div>

  {#if notifications.length === 0}
    <p class="muted">No notifications yet.</p>
  {:else}
    {#each notifications as notif}
      {@const dest = navigableDestination(notif, $familyTabGated)}
      {#snippet rowBody()}
        <span data-testid={IDS.NOTIFICATION_TYPE_ICON} class="type-icon">{notificationTypeGlyph(notif.type)}</span>
        <div class="notif-content">
          <span class="notif-summary">{notificationRowText(notif)}</span>
          <!-- notif.created_at is epoch micros (NotifItem) → ms -->
          <span class="notif-meta muted">{notif.source} · {relativeTime(notif.created_at / 1000)}</span>
        </div>
      {/snippet}
      {#if dest}
        <!-- Navigable — a real interactive control (static role/tabindex so the
             a11y linter can verify it). -->
        <div
          data-testid={IDS.NOTIFICATION_ITEM}
          class="notification-item clickable"
          class:unread={!notif.is_read}
          role="button"
          tabindex="0"
          onclick={() => openDestination(dest)}
          onkeydown={(e) => { if (e.key === 'Enter') openDestination(dest); }}
        >
          {@render rowBody()}
        </div>
      {:else}
        <!-- Inert — the shared decision has no destination: plain text, never a
             control that silently does nothing. -->
        <div data-testid={IDS.NOTIFICATION_ITEM} class="notification-item" class:unread={!notif.is_read}>
          {@render rowBody()}
        </div>
      {/if}
    {/each}
  {/if}
{/if}

<style>
  .notifications-header { display: flex; align-items: center; gap: 1rem; margin-bottom: 1rem; }
  .badge { background: var(--accent); color: white; padding: 0.25rem 0.75rem; border-radius: 1rem; font-size: 0.8rem; }
  .muted { color: var(--text-muted); }
  .notification-item { display: flex; align-items: flex-start; gap: 0.75rem; padding: 0.75rem; border-bottom: 1px solid var(--border); }
  .notification-item.clickable { cursor: pointer; }
  .notification-item.unread { font-weight: 600; background: var(--bg-surface); }
  .type-icon { font-size: 1.2rem; flex-shrink: 0; width: 1.5rem; text-align: center; }
  .notif-content { display: flex; flex-direction: column; gap: 0.15rem; }
  .notif-summary { font-size: 0.875rem; }
  .notif-meta { font-size: 0.75rem; }
</style>
