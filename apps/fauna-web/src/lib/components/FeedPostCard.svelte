<script lang="ts">
  // One feed `post-card` wired to a `FeedPostHost` — the feed's own card with
  // every child (interaction bar, badges, quoted post, media, the ⋯ menu, the
  // tip list), its verbs acting on the host's manager. The Feed page paints it
  // over the timeline; the profile's Posts tab paints it over the author window
  // (`profile.md` § Layout & flow → *The Posts tab* → *What a row is*), so the
  // two pages share one wiring rather than two copies of it.
  import PostCard from './PostCard.svelte';
  import { identity } from '#lib/store';
  import { siteLink } from '#lib/web-publish';
  import { blobUrl, contentLabelFor, type FeedPostHost } from '#lib/feed-post-host.svelte';
  import type { BridgeIdentity } from '#lib/wasm';

  interface Props {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    post: any;
    host: FeedPostHost;
    bridgeRoster: BridgeIdentity[];
    /** The host manager's in-context trained topic (the page re-derives it off
     *  its snapshot), or null — the training verbs then open the target sheet. */
    trainTarget: string | null;
    /** The card's activation — the page opens the post's detail. */
    onselect: () => void;
    /** `feed-post-report-button` — the page owns the report sheet. */
    onreport: (post_id: string) => void;
  }

  let { post, host, bridgeRoster, trainTarget, onselect, onreport }: Props = $props();
</script>

<PostCard
  {post}
  {bridgeRoster}
  decoded={host.decodedPosts[post.post_id]}
  {blobUrl}
  mediaUrl={host.mediaUrl}
  proxiedMediaUrl={host.proxiedMediaUrl}
  playbackUrl={host.playbackUrl}
  contentLabel={contentLabelFor(post)}
  oninteract={host.handleInteract}
  onrevealremote={(id) => { host.manager()?.revealRemoteImages(id); host.refreshFeed(); }}
  onbuyunlockoffer={host.buyUnlockOffer}
  onopenpaymentlink={host.openUnlockPaymentLink}
  muted={host.isPostMuted(post.post_id)}
  onrevealmuted={host.revealMuted}
  contentBlocked={host.isContentBlocked(post)}
  contentReported={host.isContentReported(post)}
  contentReason={host.guardianReasonFor(post)}
  {onreport}
  contentCollapsed={host.isContentCollapsed(post)}
  onrevealcontent={host.revealContent}
  regionPlaceholder={host.regionPlaceholderFor(post)}
  {trainTarget}
  trainMarker={host.trainMarkerFor(post.post_id, trainTarget)}
  ontrain={(id, verb) => host.trainPost(id, verb, trainTarget)}
  isOwn={post.author === $identity?.actorId}
  ondelete={host.handleDeletePost}
  onpublishweb={host.publishPostToWeb}
  onunpublishweb={host.unpublishPostFromWeb}
  oncopyweblink={host.copyPostWebLink}
  oncopypaywalllink={host.copyPostPaywallLink}
  webLinkOrigin={$siteLink?.origin ?? null}
  webLinkCopied={host.webLink.copied?.postId === post.post_id ? host.webLink.copied : null}
  {onselect}
/>
