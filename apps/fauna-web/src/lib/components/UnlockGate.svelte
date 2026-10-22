<script lang="ts">
  // The identity store's unlock gate — ui.yaml page `unlock`, shared with tui
  // (`docs/goal/architecture/apps/web.md` § Identity store at rest → *Unlock
  // comes before launch routing*). It renders IN PLACE of the whole app shell
  // until the store serves reads, so no shell code — launch routing, the
  // registry reads, the onboarding wizard — runs over a locked store.
  //
  // Modes: `create` (no seal header — choose a passphrase, typed twice),
  // `unlock` (a header and no key in this page — one passphrase, plus the
  // start-over pair a browser needs because there is no file to delete). A
  // newly opened tab first asks the tabs already unlocked for the key over the
  // `BroadcastChannel` hand-off and paints the unlock page only when none
  // answers. With a passkey wrap in the header, on a page that can ask an
  // authenticator, unlock mode also offers the passkey (`web.md` § Identity
  // store at rest → *A passkey wrap*).
  import { onDestroy, onMount, tick, type Snippet } from 'svelte';
  import {
    identityStoreGate,
    identityStoreCreateWith,
    identityStoreUnlockWith,
    identityStoreInstallHandedKey,
    identityStoreStartOverNow,
  } from '#lib/wasm-launch';
  import { requestStoreKeyFromTabs } from '#lib/identity-store';
  import { hasPasskeyWrap, passkeysPossible, unlockWithPasskey } from '#lib/identity-passkey';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let { children }: { children?: Snippet } = $props();

  // How long a new tab waits for an unlocked tab to answer before it paints
  // the unlock page — a paint delay only: an answer arriving later still
  // unlocks the page.
  const HAND_OFF_GRACE_MS = 300;

  type Gate = 'pending' | 'open' | 'create' | 'unlock' | 'failed';
  let gate = $state<Gate>('pending');
  let passphrase = $state('');
  let confirm = $state('');
  let error = $state('');
  let busy = $state(false);
  let resetConfirming = $state(false);
  let passkeyOffered = $state(false);
  let stopHandOff: (() => void) | null = null;

  function open() {
    stopHandOff?.();
    stopHandOff = null;
    passphrase = '';
    confirm = '';
    error = '';
    resetConfirming = false;
    gate = 'open';
  }

  async function decide() {
    let next;
    try {
      next = await identityStoreGate();
    } catch (e) {
      console.error('identity store: the launch chunk did not load:', e);
      error = t.unlock.error_failed({ message: String(e) });
      gate = 'failed';
      return;
    }
    if (next !== 'unlock') {
      gate = next;
      return;
    }
    if (passkeysPossible()) {
      hasPasskeyWrap().then(
        (v) => { passkeyOffered = v; },
        () => { passkeyOffered = false; },
      );
    }
    stopHandOff = requestStoreKeyFromTabs((keyHex) => {
      identityStoreInstallHandedKey(keyHex).then(open, (code) => {
        // A stale key (the store was started over since that tab unlocked)
        // is refused and changes nothing; the unlock page stays.
        console.warn('identity store: a handed key was refused:', code);
      });
    });
    setTimeout(() => {
      if (gate === 'pending') gate = 'unlock';
    }, HAND_OFF_GRACE_MS);
  }

  onMount(() => {
    void decide();
  });
  onDestroy(() => stopHandOff?.());

  async function submit() {
    if (busy || (gate !== 'create' && gate !== 'unlock')) return;
    error = '';
    if (!passphrase) {
      error = t.unlock.error_empty;
      return;
    }
    if (gate === 'create' && passphrase !== confirm) {
      error = t.unlock.error_mismatch;
      return;
    }
    busy = true;
    // The Argon2id derive blocks this thread; let the disabled button paint
    // first.
    await tick();
    await new Promise((resolve) => setTimeout(resolve, 0));
    try {
      if (gate === 'create') await identityStoreCreateWith(passphrase);
      else await identityStoreUnlockWith(passphrase);
      open();
    } catch (code) {
      error =
        code === 'wrong_passphrase_or_corrupt'
          ? t.unlock.error_wrong
          : t.unlock.error_failed({ message: String(code) });
      passphrase = '';
    } finally {
      busy = false;
    }
  }

  async function unlockPasskey() {
    if (busy || gate !== 'unlock') return;
    error = '';
    busy = true;
    try {
      await unlockWithPasskey();
      open();
    } catch (code) {
      // A ceremony the person declined is no error to report.
      if (code === 'passkey_unavailable') error = t.unlock.passkey_unavailable;
      else if (code !== 'passkey_cancelled') error = t.unlock.passkey_error({ message: String(code) });
    } finally {
      busy = false;
    }
  }

  async function startOver() {
    if (busy) return;
    busy = true;
    try {
      await identityStoreStartOverNow();
      passphrase = '';
      confirm = '';
      error = '';
      resetConfirming = false;
      passkeyOffered = false;
      gate = 'create';
    } catch (code) {
      error = t.unlock.error_failed({ message: String(code) });
    } finally {
      busy = false;
    }
  }

  function onKeydown(ev: KeyboardEvent) {
    if (ev.key === 'Enter') {
      ev.preventDefault();
      void submit();
    }
  }
</script>

{#if gate === 'open'}
  {@render children?.()}
{:else if gate !== 'pending'}
  <main class="unlock">
    <h1>
      {gate === 'create' ? t.unlock.create_title : t.unlock.unlock_title}
    </h1>
    {#if gate === 'unlock'}
      <p class="muted">{t.unlock.unlock_prompt}</p>
    {:else if gate === 'create'}
      <p class="muted">{t.unlock.create_prompt}</p>
    {/if}
    {#if gate !== 'failed'}
      <label>
        {t.unlock.passphrase_label}
        <input
          type="password"
          autocomplete={gate === 'create' ? 'new-password' : 'current-password'}
          data-testid={IDS.UNLOCK_PASSPHRASE_INPUT}
          bind:value={passphrase}
          onkeydown={onKeydown}
          disabled={busy}
        />
      </label>
      {#if gate === 'create'}
        <label>
          {t.unlock.confirm_label}
          <input
            type="password"
            autocomplete="new-password"
            data-testid={IDS.UNLOCK_CONFIRM_INPUT}
            bind:value={confirm}
            onkeydown={onKeydown}
            disabled={busy}
          />
        </label>
      {/if}
      <div class="button-group">
        <button
          class="btn primary"
          data-testid={IDS.UNLOCK_SUBMIT_BUTTON}
          onclick={() => { void submit(); }}
          disabled={busy}
        >{gate === 'create' ? t.unlock.create_button : t.unlock.unlock_button}</button>
        {#if gate === 'unlock' && passkeyOffered}
          <button
            class="btn"
            data-testid={IDS.UNLOCK_PASSKEY_BUTTON}
            onclick={() => { void unlockPasskey(); }}
            disabled={busy}
          >{t.unlock.passkey_button}</button>
        {/if}
      </div>
    {/if}
    <div data-testid={IDS.ERROR_MESSAGE} class="error" role="alert">{error}</div>
    {#if gate === 'unlock'}
      <div class="button-group reset">
        {#if resetConfirming}
          <p class="reset-explainer">{t.unlock.reset_explainer}</p>
          <button
            class="btn danger"
            data-testid={IDS.UNLOCK_RESET_CONFIRM_BUTTON}
            onclick={() => { void startOver(); }}
            disabled={busy}
          >{t.unlock.reset_confirm_button}</button>
        {:else}
          <button
            class="btn link"
            data-testid={IDS.UNLOCK_RESET_BUTTON}
            onclick={() => { resetConfirming = true; }}
            disabled={busy}
          >{t.unlock.reset_button}</button>
        {/if}
      </div>
    {/if}
  </main>
{/if}

<style>
  .unlock {
    max-width: 28rem;
    margin: 4rem auto;
    padding: 0 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .error:empty {
    display: none;
  }
  .reset {
    margin-top: 1.5rem;
    flex-wrap: wrap;
  }
  .reset-explainer {
    flex-basis: 100%;
    margin: 0 0 0.5rem;
  }
</style>
