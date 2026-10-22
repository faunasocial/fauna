<script lang="ts">
  // The shared credential-store section on web (`docs/goal/ui/settings.md`
  // § Credential store; the web mechanics are `architecture/apps/web.md`
  // § Identity store at rest). It renders only while the sealed arm is active
  // and unlocked in this page, and offers the change-passphrase modal: one
  // header write re-wraps the store key under the new passphrase, so no row is
  // rewritten and other tabs keep working on the unchanged key. On a page that
  // can ask an authenticator it also adds or removes the passkey wrap — one
  // header write each, the passphrase kept (`web.md` § Identity store at rest
  // → *A passkey wrap*).
  import { onMount, tick } from 'svelte';
  import { identityStoreSealedAndOpen, identityStoreChangePassphraseWith } from '#lib/wasm-launch';
  import { addPasskeyWrap, hasPasskeyWrap, passkeysPossible, removePasskeyWraps } from '#lib/identity-passkey';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let sealed = $state(false);
  let armed = $state(false);
  let current = $state('');
  let next = $state('');
  let confirm = $state('');
  let error = $state('');
  let busy = $state(false);
  let success = $state(false);
  // `null` until read, and on a page that cannot ask an authenticator: no
  // passkey control is painted.
  let passkey = $state<boolean | null>(null);

  onMount(() => {
    identityStoreSealedAndOpen().then(
      (v) => {
        sealed = v;
        if (v && passkeysPossible()) {
          hasPasskeyWrap().then(
            (has) => { passkey = has; },
            () => { passkey = null; },
          );
        }
      },
      () => { sealed = false; },
    );
  });

  async function changePasskey(add: boolean) {
    if (busy) return;
    error = '';
    busy = true;
    try {
      if (add) await addPasskeyWrap();
      else await removePasskeyWraps();
      passkey = await hasPasskeyWrap();
    } catch (code) {
      // A ceremony the person declined is no error to report.
      if (code === 'passkey_unavailable') error = t.unlock.passkey_unavailable;
      else if (code !== 'passkey_cancelled') error = t.credential_store.passkey_error({ message: String(code) });
    } finally {
      busy = false;
    }
  }

  function disarm() {
    armed = false;
    current = '';
    next = '';
    confirm = '';
    error = '';
  }

  function arm() {
    disarm();
    success = false;
    armed = true;
  }

  async function submit() {
    if (busy) return;
    error = '';
    if (!current || !next) {
      error = t.credential_store.error_empty;
      return;
    }
    if (next !== confirm) {
      error = t.credential_store.error_mismatch;
      return;
    }
    busy = true;
    // Two Argon2id derives block this thread; let the disabled button paint.
    await tick();
    await new Promise((resolve) => setTimeout(resolve, 0));
    try {
      await identityStoreChangePassphraseWith(current, next);
      disarm();
      success = true;
    } catch (code) {
      error =
        code === 'wrong_passphrase_or_corrupt'
          ? t.credential_store.error_wrong
          : t.credential_store.error_failed({ message: String(code) });
    } finally {
      busy = false;
    }
  }
</script>

{#if sealed}
  <section class="section" data-testid={IDS.CREDENTIAL_STORE_SECTION}>
    <h2>{t.credential_store.section_title}</h2>
    <p class="muted" data-testid={IDS.CREDENTIAL_STORE_STATUS}>{t.credential_store.status_sealed}</p>
    <button class="btn" data-testid={IDS.CREDENTIAL_STORE_REKEY_BUTTON} onclick={arm} disabled={busy}>
      {t.credential_store.rekey_button}
    </button>
    {#if passkey === false}
      <button class="btn" data-testid={IDS.CREDENTIAL_STORE_PASSKEY_ADD_BUTTON} onclick={() => { void changePasskey(true); }} disabled={busy}>
        {t.credential_store.passkey_add_button}
      </button>
      <p class="muted">{t.credential_store.passkey_explainer}</p>
    {:else if passkey === true}
      <button class="btn" data-testid={IDS.CREDENTIAL_STORE_PASSKEY_REMOVE_BUTTON} onclick={() => { void changePasskey(false); }} disabled={busy}>
        {t.credential_store.passkey_remove_button}
      </button>
      <p class="muted">{t.credential_store.passkey_status}</p>
    {/if}
    {#if success}
      <p class="muted" data-testid={IDS.CREDENTIAL_STORE_REKEY_SUCCESS}>{t.credential_store.success}</p>
    {/if}
    {#if armed}
      <div class="rekey" data-testid={IDS.CREDENTIAL_STORE_REKEY_MODAL} role="dialog" aria-label={t.credential_store.rekey_title}>
        <h3>{t.credential_store.rekey_title}</h3>
        <!-- The nudge sits ABOVE the inputs: the reader learns why a backup
             matters before choosing the new passphrase. -->
        <p class="muted" data-testid={IDS.CREDENTIAL_STORE_REKEY_SEED_NUDGE}>{t.credential_store.seed_nudge}</p>
        <label>
          {t.credential_store.current_label}
          <input type="password" autocomplete="current-password" data-testid={IDS.CREDENTIAL_STORE_REKEY_CURRENT_INPUT} bind:value={current} disabled={busy} />
        </label>
        <label>
          {t.credential_store.new_label}
          <input type="password" autocomplete="new-password" data-testid={IDS.CREDENTIAL_STORE_REKEY_NEW_INPUT} bind:value={next} disabled={busy} />
        </label>
        <label>
          {t.credential_store.confirm_label}
          <input type="password" autocomplete="new-password" data-testid={IDS.CREDENTIAL_STORE_REKEY_CONFIRM_INPUT} bind:value={confirm} disabled={busy} />
        </label>
        <div class="button-group">
          <button class="btn primary" data-testid={IDS.CREDENTIAL_STORE_REKEY_SUBMIT_BUTTON} onclick={() => { void submit(); }} disabled={busy}>
            {t.credential_store.submit}
          </button>
          <button class="btn" data-testid={IDS.CREDENTIAL_STORE_REKEY_CANCEL_BUTTON} onclick={disarm} disabled={busy}>
            {t.common.cancel}
          </button>
        </div>
      </div>
    {/if}
    {#if error}<p class="error" data-testid={IDS.ERROR_MESSAGE}>{error}</p>{/if}
  </section>
{/if}

<style>
  .rekey {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
</style>
