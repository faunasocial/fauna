<script lang="ts">
  // A bridged post's own picture in the `post-image` slot (render-model.md § D6c). The page
  // fetches its nest-relative `path` with the session bearer and hands down an object URL as
  // `src` (`null` while in flight or after a failure); until then the element is a placeholder
  // whose text is the path — the observable tui's label and apple's placeholder carry. No
  // C2PA badge: the bytes are the proxy's live answer, never a stored blob the check could
  // address. A click opens the lightbox, as on a blob image.
  import Lightbox from './Lightbox.svelte';
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    path: string;
    /** The block's own `alt` — painted when non-empty, the fixed description otherwise. */
    alt?: string;
    src: string | null;
    class?: string;
  }

  let { path, alt = '', src, class: className = '' }: Props = $props();
  const label = $derived(alt || 'post media');
  let showLightbox = $state(false);
</script>

{#if src}
  <button type="button" class="proxied-image" onclick={() => { showLightbox = true; }}>
    <img data-testid={IDS.POST_IMAGE} data-proxied-path={path} {src} alt={label} class={className} />
  </button>
  {#if showLightbox}
    <Lightbox {src} alt={label} onclose={() => { showLightbox = false; }} />
  {/if}
{:else}
  <span data-testid={IDS.POST_IMAGE} data-proxied-path={path} class="proxied-placeholder" title={label}>{path}</span>
{/if}

<style>
  .proxied-image {
    border: none;
    padding: 0;
    background: none;
    cursor: pointer;
  }
  .proxied-placeholder {
    display: inline-block;
    max-width: 100%;
    overflow-wrap: anywhere;
    font-size: 0.8rem;
    color: var(--muted, #888);
  }
</style>
