<script lang="ts">
  // Every trusted media block of a post, in body order (render-model.md § D6b *Accessors*):
  // one `post-image` per picture block and one `video-thumbnail` per video block of the shared
  // fold's `media_blocks`, each labelled with its block's `alt`, or the shared
  // `feed.post.image_alt_fallback` / `video_alt_fallback` string when the author wrote none
  // (§ D6c → *Proxied video*, the `alt` paragraph). The list card and the post detail paint
  // this one component, so the two surfaces cannot drift apart. The image-vs-video branch is
  // made once, in shared Rust; this only dispatches on the typed block.
  import C2paImage from './C2paImage.svelte';
  import ProxiedImage from './ProxiedImage.svelte';
  import VideoThumbnail from './VideoThumbnail.svelte';
  import { documentMediaBlocks, mediaBlockKey, type RenderDocument } from '#lib/document';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  interface Props {
    document: RenderDocument | undefined;
    /** The post the blocks belong to — what `mediaUrl` / `playbackUrl` unseal against. */
    post: unknown;
    blobUrl: (hash: string) => string;
    /** A post-media blob → something an `<img>` can show: the plain blob URL, or a local
     *  object URL for an item that had to be unsealed (media.md § Encryption at rest);
     *  `null` while a sealed item's bytes are in flight, or if it did not open — paint nothing. */
    mediaUrl: (hash: string, post?: unknown) => string | null;
    /** A bridged `ProxiedImage` path → the object URL of its bytes (render-model.md § D6c);
     *  `null` while in flight or after a failure. Absent → the path placeholder only. */
    proxiedMediaUrl?: (path: string) => string | null;
    /** What a tapped `video-thumbnail` plays (render-model.md § D6c → *Inline playback*). */
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    playbackUrl?: (block: any, post?: unknown) => Promise<string | null>;
    postId?: string;
    /** The image elements' class — the card's thumbnail size or the detail's full width. */
    imageClass?: string;
    /** C2PA thumbnail badge mode (the list card's). */
    thumb?: boolean;
  }

  let {
    document,
    post,
    blobUrl,
    mediaUrl,
    proxiedMediaUrl,
    playbackUrl,
    postId,
    imageClass = '',
    thumb = false,
  }: Props = $props();

  let blocks = $derived(documentMediaBlocks(document));
  const imageAlt = (alt: string | undefined) => alt || t.feed.post.image_alt_fallback;
  const videoAlt = (alt: string | undefined) => alt || t.feed.post.video_alt_fallback;
</script>

{#if blocks.length > 0}
  <div class="post-media">
    {#each blocks as block, i (mediaBlockKey(block, i))}
      {#if 'Image' in block}
        <!-- `mediaUrl`, not `blobUrl`: a tier-restricted post's attachment is sealed under the
             post's own per-post key and no URL can render it, so the page hands back a local
             object URL for those and the plain blob URL for everything else. `null` = in
             flight or would not open. -->
        {@const mediaSrc = mediaUrl(block.Image.hash, post)}
        {#if mediaSrc}
          <C2paImage data-testid={IDS.POST_IMAGE} data-blob-hash={block.Image.hash} src={mediaSrc} alt={imageAlt(block.Image.alt)} class={imageClass} {thumb} />
        {/if}
      {:else if 'Video' in block}
        <VideoThumbnail
          src={blobUrl(block.Video.hash)}
          alt={videoAlt(block.Video.alt)}
          resolveSource={playbackUrl ? () => playbackUrl(block, post) : undefined}
          {postId}
        />
      {:else if 'ProxiedImage' in block}
        <!-- A bridged post's own picture (render-model.md § D6c): an authenticated fetch of its
             nest-relative path → object URL, never a bare `<img src>` to the path. -->
        <ProxiedImage
          path={block.ProxiedImage.path}
          alt={block.ProxiedImage.alt}
          src={proxiedMediaUrl ? proxiedMediaUrl(block.ProxiedImage.path) : null}
          class={imageClass}
        />
      {:else if 'ProxiedVideo' in block}
        <!-- A bridged post's own video (render-model.md § D6c → *Proxied video*): the play glyph
             over a poster-less frame carrying the path — never byte-loaded before the tap. -->
        <VideoThumbnail
          caption={block.ProxiedVideo.path}
          alt={videoAlt(block.ProxiedVideo.alt)}
          resolveSource={playbackUrl ? () => playbackUrl(block, post) : undefined}
          {postId}
        />
      {/if}
    {/each}
  </div>
{/if}

<style>
  .post-media { margin-top: 0.5rem; }
</style>
