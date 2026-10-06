<script lang="ts">
  import { browser } from '$app/environment';
  import { onMount, onDestroy } from 'svelte';
  import { logMessage } from '$lib/wasm';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  let { error = $bindable(''), warning = $bindable(''), info = $bindable('') } = $props();

  // Test builds only (testing.md § Test-agent build exclusion): mirror the
  // banner texts onto `window.__fauna_messages` for the e2e bridge. Stripped
  // from production bundles by the constant-folded flag.
  $effect(() => {
    if (__FAUNA_E2E_AUTOMATION__ && browser) {
      (window as any).__fauna_messages = {
        error: error || null,
        warning: warning || null,
        info: info || null,
      };
    }
  });

  // Category 1 (observability.md § What must be logged): the display funnel
  // feeds the shared `fauna_log` ring as a banner appears — once per distinct
  // message, at the producer (the prop *change*), not the reactive paint
  // (§ "Log on the *event*, not the *paint*"). Tracking the previous value keeps
  // a re-render of an unchanged banner from re-logging. Level matches the banner
  // kind (error→error, warning→warn, info/success→info). The banner text is
  // operational copy — the funnel logs the message it shows (no plaintext bodies
  // or secrets reach a banner; § Persistence & privacy redaction rule).
  let prevError = '', prevWarning = '', prevInfo = '';
  $effect(() => {
    if (!browser) return;
    if (error && error !== prevError) logMessage('error', 'fauna_web::banner', error);
    if (warning && warning !== prevWarning) logMessage('warn', 'fauna_web::banner', warning);
    if (info && info !== prevInfo) logMessage('info', 'fauna_web::banner', info);
    prevError = error;
    prevWarning = warning;
    prevInfo = info;
  });

  // Listen for `fauna-message-update` window events — the shared shell message
  // channel: both the test agent's `set_state` message injection and production
  // cross-page surfaces (e.g. the post-onboarding "off-box recovery custody not
  // saved" warning the admin-claim launch glue dispatches after navigating to the
  // feed, where this banner is mounted in the root layout's content shell).
  let handler: ((e: Event) => void) | null = null;
  onMount(() => {
    if (browser) {
      // Live mount count (not just "has __fauna_messages ever been set" — that
      // object outlives navigation to a bannerless page like onboarding): the
      // e2e test agent's getState() reads this to decide whether `messages` is
      // a real object or `null`, so a page with no MessageBanner instance falls
      // back to the DOM `error-message` element instead of a stale `{error:
      // null, ...}` shadowing it forever (testing.md § convention 2).
      //
      // Behind `__FAUNA_E2E_AUTOMATION__` because it is exactly that — an e2e
      // hook — and convention 15 keeps the whole `window.__fauna_*` surface out
      // of shipped artifacts. It was bare here until 2026-09-02 and therefore
      // shipped: a `grep -rl "__fauna_" /usr/share/fauna-web` inside
      // `ghcr.io/faunasocial/nest:latest` found this one name (and only this
      // one) in the production SPA. The guard is the same in-component idiom
      // `$lib/onboarding/machine.svelte.ts` uses for the hooks that must run at
      // module load; a production `vite build` folds it to false and strips the
      // block.
      if (__FAUNA_E2E_AUTOMATION__) {
        (window as unknown as { __fauna_message_banner_mount_count?: number })
          .__fauna_message_banner_mount_count =
          ((window as unknown as { __fauna_message_banner_mount_count?: number })
            .__fauna_message_banner_mount_count ?? 0) + 1;
      }
      handler = (e: Event) => {
        const detail = (e as CustomEvent).detail;
        if (detail.error !== undefined) error = detail.error;
        if (detail.warning !== undefined) warning = detail.warning;
        if (detail.info !== undefined) info = detail.info;
      };
      window.addEventListener('fauna-message-update', handler);
    }
  });
  onDestroy(() => {
    if (browser && __FAUNA_E2E_AUTOMATION__) {
      // Same guard as the increment in `onMount` — gating one half only would
      // leave the counter climbing forever in a test build.
      const w = window as unknown as { __fauna_message_banner_mount_count?: number };
      w.__fauna_message_banner_mount_count = Math.max(0, (w.__fauna_message_banner_mount_count ?? 1) - 1);
    }
    if (handler) window.removeEventListener('fauna-message-update', handler);
  });
</script>

{#if error}
<div class="message-banner error-banner" role="alert">
  <span class="banner-text" data-testid={IDS.ERROR_MESSAGE}>{error}</span>
  <button class="dismiss-btn" onclick={() => error = ''} aria-label={t.common.dismiss}>&times;</button>
</div>
{/if}

{#if warning}
<div class="message-banner warning-banner" role="alert">
  <span class="banner-text" data-testid={IDS.WARNING_MESSAGE}>{warning}</span>
  <button class="dismiss-btn" onclick={() => warning = ''} aria-label={t.common.dismiss}>&times;</button>
</div>
{/if}

{#if info}
<div class="message-banner info-banner" role="status">
  <span class="banner-text" data-testid={IDS.INFO_MESSAGE}>{info}</span>
  <button class="dismiss-btn" onclick={() => info = ''} aria-label={t.common.dismiss}>&times;</button>
</div>
{/if}

<style>
  .message-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
    margin-bottom: 0.75rem;
    font-size: 0.875rem;
    gap: 0.5rem;
  }
  .error-banner {
    background: color-mix(in srgb, var(--danger, #ef4444) 15%, transparent);
    border: 1px solid var(--danger, #ef4444);
    color: var(--danger, #ef4444);
  }
  .warning-banner {
    background: color-mix(in srgb, #f59e0b 15%, transparent);
    border: 1px solid #f59e0b;
    color: #d97706;
  }
  .info-banner {
    background: color-mix(in srgb, var(--accent, #3b82f6) 15%, transparent);
    border: 1px solid var(--accent, #3b82f6);
    color: var(--accent, #3b82f6);
  }
  .banner-text { flex: 1; }
  .dismiss-btn {
    background: none;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.25rem;
    color: inherit;
    opacity: 0.7;
    flex-shrink: 0;
  }
  .dismiss-btn:hover { opacity: 1; }
</style>
