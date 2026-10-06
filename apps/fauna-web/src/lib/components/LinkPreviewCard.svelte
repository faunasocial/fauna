<script lang="ts">
  /** The resolved `RenderBlock::LinkPreview` card (render-model.md § D4). The feed
   *  page extracts the document's `LinkPreview` blocks, fires
   *  `manager.resolveLinkPreview(url)` for the `Resolving` ones, and paints this
   *  card for each `Resolved` one (`Resolving`/`Failed` paint nothing — the inline
   *  link in the body paragraph already shows). The whole card is the clickable
   *  link to `url`. `imageUrl` is the og:image blob URL (built by the page from
   *  `image_hash` via the `/api/v1/blob/<hash>` path) or `null` when there is no
   *  usable preview image **or it is still blocked-by-default** (render-model.md § D4,
   *  user-ratified 2026-06-27): the og:image obeys the post's D3 remote-content reveal,
   *  so the caller passes `imageUrl` only once the post is revealed — title/description/
   *  domain always show, the image appears after the post's reveal button is tapped. The
   *  domain is the shared `urlHost` (wasm twin of `fauna_core::format::url_host`) — the
   *  same source of truth linux/tui/android/apple/windows already consume. */
  import { urlHost } from '$lib/wasm';
  import { IDS } from '$lib/generated/uiIds';

  interface Props {
    url: string;
    title: string;
    description: string;
    imageUrl: string | null;
  }

  let { url, title, description, imageUrl }: Props = $props();

  const domain = $derived(urlHost(url));
</script>

<a
  class="link-preview-card"
  data-testid={IDS.LINK_PREVIEW_CARD}
  href={url}
  target="_blank"
  rel="noopener noreferrer"
>
  {#if imageUrl}
    <img class="link-preview-image" data-testid={IDS.LINK_PREVIEW_IMAGE} src={imageUrl} alt="" />
  {/if}
  <div class="link-preview-text">
    {#if title}
      <span class="link-preview-title" data-testid={IDS.LINK_PREVIEW_TITLE}>{title}</span>
    {/if}
    {#if description}
      <span class="link-preview-description" data-testid={IDS.LINK_PREVIEW_DESCRIPTION}>{description}</span>
    {/if}
    <span class="link-preview-domain" data-testid={IDS.LINK_PREVIEW_DOMAIN}>{domain}</span>
  </div>
</a>

<style>
  .link-preview-card {
    display: block;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
    margin-top: 0.5rem;
    background: var(--bg-surface);
    text-decoration: none;
    color: inherit;
  }
  .link-preview-image {
    display: block;
    width: 100%;
    max-height: 220px;
    object-fit: cover;
  }
  .link-preview-text {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding: 0.5rem 0.75rem;
  }
  .link-preview-title {
    font-size: 0.85rem;
    font-weight: 600;
  }
  .link-preview-description {
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .link-preview-domain {
    font-size: 0.72rem;
    color: var(--text-muted);
    text-transform: lowercase;
  }
</style>
