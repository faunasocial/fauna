<script lang="ts">
  import { onMount } from "svelte";
  import { readProvenanceFromUrl, readProvenanceFromBytes, type C2paResult } from "$lib/c2pa";
  import { displaySrcFor } from "$lib/media-src";
  import Lightbox from "./Lightbox.svelte";
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    src: string;
    alt?: string;
    class?: string;
    bytes?: Uint8Array;
    mimeType?: string;
    thumb?: boolean;
    [key: string]: any;
  }

  let { src, alt = "", class: className = "", bytes, mimeType, thumb = false, ...rest }: Props = $props();

  // Use thumbnail URL for display, full URL for C2PA detection. The `?thumb=1`
  // rule (and why a `blob:` object URL is exempt from it) lives in `$lib/media-src`
  // so it can be tested.
  const displaySrc = $derived(displaySrcFor(src, thumb));

  let c2pa: C2paResult | null = $state(null);
  let checked = $state(false);
  let showPopover = $state(false);
  let showLightbox = $state(false);

  onMount(async () => {
    try {
      if (bytes && mimeType) {
        c2pa = await readProvenanceFromBytes(bytes, mimeType);
      } else {
        c2pa = await readProvenanceFromUrl(src);
      }
    } catch {
      // Silently fail — image still renders fine without badge
    }
    checked = true;
  });

  function togglePopover(e: MouseEvent) {
    e.stopPropagation();
    showPopover = !showPopover;
  }

  function closePopover() {
    showPopover = false;
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") closePopover();
  }

  function formatDate(iso: string): string {
    if (!iso) return t.common.unknown;
    try {
      return new Date(iso).toLocaleDateString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
      });
    } catch {
      return iso;
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} onclick={closePopover} />

{#if checked && c2pa}
  <div class="c2pa-wrap {className}">
    <img src={displaySrc} {alt} class="c2pa-img" {...rest} onclick={() => { showLightbox = true; }} style="cursor: pointer;" />
    <button
      data-testid={IDS.C2PA_BADGE}
      class="c2pa-badge"
      class:invalid={!c2pa.isValid}
      onclick={togglePopover}
      title={c2pa.isValid ? t.c2pa.verified_title : t.c2pa.invalid_title}
      aria-label={t.c2pa.view_label}
    >
      {#if c2pa.isValid}
        <svg viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg">
          <path d="M6 10l3 3 5-6" stroke="white" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      {:else}
        <svg viewBox="0 0 20 20" fill="none" xmlns="http://www.w3.org/2000/svg">
          <path d="M10 6v4m0 3h.01" stroke="white" stroke-width="2" stroke-linecap="round"/>
        </svg>
      {/if}
    </button>
    {#if showPopover}
      <div class="c2pa-popover" role="dialog" tabindex="-1" aria-label={t.c2pa.provenance} onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
        <div class="c2pa-popover-title">{t.c2pa.provenance}</div>
        <dl class="c2pa-popover-details">
          <dt>{t.c2pa.signer}</dt>
          <dd>{c2pa.signer}</dd>
          <dt>{t.backups.date}</dt>
          <dd>{formatDate(c2pa.signingDate)}</dd>
          <dt>{t.c2pa.tool}</dt>
          <dd>{c2pa.claimGenerator}</dd>
          <dt>{t.common.status}</dt>
          <dd class:valid={c2pa.isValid} class:invalid={!c2pa.isValid}>
            {c2pa.isValid ? t.c2pa.valid : t.c2pa.validation_issue}
          </dd>
        </dl>
      </div>
    {/if}
  </div>
{:else}
  <img src={displaySrc} {alt} class={className} {...rest} onclick={() => { showLightbox = true; }} style="cursor: pointer;" />
{/if}

{#if showLightbox}
  <Lightbox src={src} {alt} onclose={() => { showLightbox = false; }} />
{/if}

<style>
  .c2pa-wrap {
    position: relative;
    display: inline-block;
  }
  .c2pa-img {
    display: block;
    max-width: 100%;
    height: auto;
  }
  .c2pa-badge {
    position: absolute;
    bottom: 8px;
    right: 8px;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: rgba(37, 99, 235, 0.9);
    border: 2px solid white;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.3);
    transition: transform 0.15s ease;
  }
  .c2pa-badge:hover {
    transform: scale(1.15);
  }
  .c2pa-badge.invalid {
    background: rgba(234, 179, 8, 0.9);
  }
  .c2pa-badge svg {
    width: 14px;
    height: 14px;
  }
  .c2pa-popover {
    position: absolute;
    bottom: 40px;
    right: 0;
    background: var(--bg-surface, #fff);
    border: 1px solid var(--border, #e5e7eb);
    border-radius: 8px;
    padding: 0.75rem;
    min-width: 220px;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.15);
    z-index: 100;
    font-size: 0.85rem;
  }
  .c2pa-popover-title {
    font-weight: 600;
    margin-bottom: 0.5rem;
    font-size: 0.9rem;
  }
  .c2pa-popover-details {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0;
  }
  .c2pa-popover-details dt {
    color: var(--text-muted, #6b7280);
    font-size: 0.8rem;
  }
  .c2pa-popover-details dd {
    margin: 0;
    word-break: break-word;
  }
  .valid {
    color: #16a34a;
  }
  .invalid {
    color: #d97706;
  }
</style>
