<script lang="ts">
  // The `video-thumbnail` slot IS the player host (render-model.md § D6c → *Inline
  // playback*): idle it is today's poster-less `<video preload="metadata">` under a
  // play glyph; a tap asks the page what plays (`resolveSource` — the shared
  // `FeedManager::playback_source` decision, turned into a `<video src>`) and swaps the
  // glyph for the browser's native player in the same element. Never autoplay: the tap
  // is what spends the bytes. `data-state` (`idle` / `loading` / `playing` / `error`)
  // is the headless observable every player test reads; the native player's own error
  // UI is the error surface — no new element.
  import { IDS } from '$lib/generated/uiIds';
  interface Props {
    /** The blob URL whose first frame is the poster (`<video preload="metadata">`). Absent for a
     *  bridged `ProxiedVideo` (render-model.md § D6c → *Proxied video*): its first-frame grab
     *  would be a full bearer fetch per card, so it paints a poster-less frame instead. */
    src?: string;
    /** Text painted in a poster-less frame — a `ProxiedVideo`'s nest-relative path, the
     *  observable every app's `video-thumbnail` carries for it. */
    caption?: string;
    alt?: string;
    /** Resolve the playable URL for this video, `null` when nothing plays. Absent → the
     *  thumbnail stays inert (no host wired the projection). */
    resolveSource?: () => Promise<string | null>;
  }

  let { src, caption, alt = 'video', resolveSource }: Props = $props();

  type PlayState = 'idle' | 'loading' | 'playing' | 'error';
  let playState = $state<PlayState>('idle');
  let playSrc = $state<string | null>(null);
  let player = $state<HTMLVideoElement | null>(null);

  async function activate() {
    if (!resolveSource || (playState !== 'idle' && playState !== 'error')) return;
    playState = 'loading';
    let url: string | null = null;
    try {
      url = await resolveSource();
    } catch {
      url = null;
    }
    if (!url) {
      playState = 'error';
      return;
    }
    playSrc = url;
  }

  // Start the native player once it is mounted on the resolved source. A refused
  // `play()` (a browser that lost the tap's activation across the await) leaves the
  // native controls up for the user's own press, which fires `playing` all the same.
  $effect(() => {
    if (player && playSrc) {
      player.play().catch(() => {});
    }
  });
</script>

{#if playSrc}
  <div class="video-thumbnail" data-testid={IDS.VIDEO_THUMBNAIL} data-state={playState}>
    <video
      bind:this={player}
      src={playSrc}
      controls
      playsinline
      class="video-player"
      aria-label={alt}
      onplaying={() => (playState = 'playing')}
      onerror={() => (playState = 'error')}
    >
      <track kind="captions" />
    </video>
  </div>
{:else}
  <button
    class="video-thumbnail"
    data-testid={IDS.VIDEO_THUMBNAIL}
    data-state={playState}
    type="button"
    aria-label={alt}
    onclick={activate}
  >
    {#if src}
      <video {src} preload="metadata" class="video-preview">
        <track kind="captions" />
      </video>
      <span class="play-overlay">▶</span>
    {:else}
      <span class="video-frame"><span class="play-glyph">▶</span> {caption ?? ''}</span>
    {/if}
  </button>
{/if}

<style>
  .video-thumbnail {
    position: relative;
    display: inline-block;
    border: none;
    padding: 0;
    background: none;
    cursor: pointer;
    border-radius: 6px;
    overflow: hidden;
  }
  div.video-thumbnail {
    cursor: default;
  }
  .video-preview,
  .video-player {
    max-width: 100%;
    max-height: 300px;
    display: block;
    border-radius: 6px;
  }
  .video-frame {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    max-width: 100%;
    padding: 0.5rem 0.75rem;
    overflow-wrap: anywhere;
    font-size: 0.8rem;
    color: var(--muted, #888);
    background: rgba(0, 0, 0, 0.05);
    border-radius: 6px;
  }
  .play-overlay {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    font-size: 2rem;
    color: white;
    background: rgba(0, 0, 0, 0.5);
    border-radius: 50%;
    width: 3rem;
    height: 3rem;
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
  }
</style>
