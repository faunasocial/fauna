<script lang="ts">
  // `src` absent → the placeholder form: a picture whose bytes this device does not
  // hold still opens, showing its DECLARED text (`placeholder`, filename and size)
  // in place of the paint — never a dropped click (conversations.md § User actions,
  // the `dm-attachment-image[i]` row; tui's and windows' placeholder form).
  interface Props {
    src?: string;
    alt?: string;
    placeholder?: string;
    onclose: () => void;
  }

  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let { src, alt = "", placeholder = "", onclose }: Props = $props();

  let overlayEl: HTMLDivElement | undefined = $state();

  // Move focus into the dialog when it opens so Escape and screen readers work.
  $effect(() => { overlayEl?.focus(); });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
</script>

<div
  bind:this={overlayEl}
  class="lightbox-overlay"
  data-testid={IDS.IMAGE_LIGHTBOX}
  role="dialog"
  aria-modal="true"
  aria-label={t.c2pa.image_viewer}
  tabindex="-1"
  onclick={(e) => { if (e.target === e.currentTarget) onclose(); }}
  onkeydown={handleKeydown}
>
  <button class="lightbox-close" onclick={onclose} aria-label={t.common.close}>&times;</button>
  {#if src}
    <img {src} {alt} class="lightbox-img" />
  {:else}
    <p class="lightbox-placeholder">🖼 {placeholder}</p>
  {/if}
</div>

<style>
  .lightbox-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.85);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    cursor: pointer;
    outline: none;
  }
  .lightbox-img {
    max-width: 90vw;
    max-height: 90vh;
    object-fit: contain;
    border-radius: 4px;
    cursor: default;
  }
  .lightbox-placeholder {
    color: white;
    font-size: 1.25rem;
    cursor: default;
    padding: 2rem;
    text-align: center;
  }
  .lightbox-close {
    position: absolute;
    top: 1rem;
    right: 1rem;
    background: none;
    border: none;
    color: white;
    font-size: 2rem;
    cursor: pointer;
    line-height: 1;
    padding: 0.25rem 0.5rem;
  }
  .lightbox-close:hover {
    opacity: 0.7;
  }
</style>
