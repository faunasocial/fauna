<script lang="ts">
  // The Stage-2 in-app re-auth confirm (`account-activate-reauth-prompt`,
  // `long-term-store.md` § Multi-account evolution → *Per-account re-auth*) —
  // the shape the linux slice ratified for the no-native-prompt platforms,
  // mirroring the file-version-restore-confirm-modal family. A confirmation,
  // not a credential check; cancel / backdrop / Escape are all the decline
  // path, a pure no-op the caller owns. One component for every switcher web
  // paints it from: Settings' account switcher and `launch_retry`'s.
  import { IDS } from '#lib/generated/uiIds';
  import { t } from '#lib/i18n/strings';

  let {
    label,
    busy = false,
    onconfirm,
    oncancel,
  }: {
    /** The target account's display label, named in the prompt body. */
    label: string;
    /** A switch is in flight: the confirm button is disabled. */
    busy?: boolean;
    onconfirm: () => void;
    oncancel: () => void;
  } = $props();
</script>

<svelte:window onkeydown={(e) => { if (e.key === 'Escape') oncancel(); }} />

<div class="modal-backdrop" role="presentation" onclick={oncancel}>
  <div
    data-testid={IDS.ACCOUNT_ACTIVATE_REAUTH_PROMPT}
    class="modal"
    role="dialog"
    aria-modal="true"
    onclick={(e) => e.stopPropagation()}
  >
    <h2>{t.settings.account_page.reauth_prompt_title}</h2>
    <p>{t.settings.account_page.reauth_prompt_body({ account: label })}</p>
    <div class="modal-actions">
      <button
        data-testid={IDS.ACCOUNT_ACTIVATE_REAUTH_CANCEL_BUTTON}
        class="btn"
        onclick={oncancel}
      >{t.common.cancel}</button>
      <button
        data-testid={IDS.ACCOUNT_ACTIVATE_REAUTH_CONFIRM_BUTTON}
        class="btn primary"
        onclick={onconfirm}
        disabled={busy}
      >{t.settings.account_page.reauth_confirm}</button>
    </div>
  </div>
</div>

<style>
  .modal-backdrop {
    position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4);
    display: flex; align-items: center; justify-content: center; z-index: 100;
  }
  .modal {
    background: var(--bg, #fff); padding: 1.5rem; border-radius: 8px;
    max-width: 560px; max-height: 80vh; overflow-y: auto;
  }
  .modal-actions { display: flex; gap: 0.5rem; justify-content: flex-end; margin-top: 1rem; }
</style>
