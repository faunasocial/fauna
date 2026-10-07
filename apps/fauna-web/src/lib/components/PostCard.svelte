<script lang="ts">
  import C2paImage from './C2paImage.svelte';
  import ContentLabelBadge from './ContentLabelBadge.svelte';
  import InteractionBar from './InteractionBar.svelte';
  import ProtocolBadge from './ProtocolBadge.svelte';
  import UnverifiedSourceBadge from './UnverifiedSourceBadge.svelte';
  import DelegatedOriginBadge from './DelegatedOriginBadge.svelte';
  import QuotedPost from './QuotedPost.svelte';
  import LinkPreviewCard from './LinkPreviewCard.svelte';
  import ProxiedImage from './ProxiedImage.svelte';
  import VideoThumbnail from './VideoThumbnail.svelte';
  import TipSurface from './payments/TipSurface.svelte';
  import RegionPlaceholder from './RegionPlaceholder.svelte';
  import { markdownToHtml } from '$lib/markdown';
  import { shortActor, liveStatusClass } from '$lib/feed-utils';
  import { structuredView, markdownToDocument, type RegionPlaceholder as RegionPlaceholderValue } from '$lib/wasm';
  import { documentToHtml, documentHasBlockedRemoteImages, quotedPostBlock, documentMediaBlocks, resolvedLinkPreviews } from '$lib/document';
  import { t } from '$lib/i18n/strings';
  import { relativeTime } from '$lib/value-format';
  import { IDS } from '$lib/generated/uiIds';

  /** A snapshot `fauna_feed::PostSummary` — the shared post-list row
   *  (`feed.md` § State & data shape). `body` is the nest's preview; `tags` /
   *  `has_media` / `media_hash` / `quoted_post_id` / `source` drive the chips,
   *  image, embed, and badges. The optional `decoded` body is the web rich-render
   *  augmentation (structured cards + markdown + multi-media), kept per
   *  `feed.md` § Where logic lives (web rides `structuredView`). */
  interface Props {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    post: any;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    decoded: any;
    blobUrl: (hash: string) => string;
    /** Resolve a post-media blob to something an `<img>` can show. Returns the plain
     *  blob URL for the ordinary case and a local object URL for an item that had to be
     *  unsealed (media.md § Encryption at rest); `null` while a sealed item's bytes are
     *  still in flight, or if it did not open — paint nothing, as for any undecodable
     *  image. The card cannot reach the feed manager, so the page decides which of the
     *  two it is and hands down the result — the same shape as `onrevealremote`. */
    mediaUrl: (hash: string) => string | null;
    /** A bridged post's `ProxiedImage` path → the object URL of its bytes, fetched by the page
     *  with the session bearer (render-model.md § D6c); `null` while in flight or after a
     *  failure — the card then paints the path placeholder. Absent → placeholder only. */
    proxiedMediaUrl?: (path: string) => string | null;
    /** What a tapped `video-thumbnail` plays (render-model.md § D6c → *Inline playback*): the
     *  page asks the shared manager's `playbackSource` and turns its answer into a `<video src>`
     *  (a nest URL, or a sealed item's object URL). `null` = nothing playable. On the page for
     *  `mediaUrl`'s reason: the card cannot reach the feed manager. */
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    playbackUrl?: (block: any) => Promise<string | null>;
    oninteract: (post_id: string, action: string) => void;
    contentLabel?: string;
    /** Open the post-detail view (ui.yaml feed transition `click post-card → post_detail`). */
    onselect?: () => void;
    /** Opt this post's blocked remote images into fetching — the card can't reach the feed
     *  manager directly, so the parent feed page dispatches `revealRemoteImages(post_id)` +
     *  refreshes (render-model.md § D3). Mirrors the `oninteract` callback shape. */
    onrevealremote?: (post_id: string) => void;

    // ── The self-serve teaser purchase (gap (2c), monetization.md § Per-post
    //    pay-to-unlock → the buyer's price read is post-addressed). The card
    //    can't reach the manager, so the parent dispatches on `post.post_id`. ──
    onbuyunlockoffer?: (post_id: string) => void;
    /** Open the resolved offer's payment_url — the parent owns the
     *  https-only safety check + window.open (profile page's precedent). */
    onopenpaymentlink?: (url: string) => void;

    // ── Muted-keyword collapse (topic-factors.md § Scoring: a mute collapses
    // EVERYWHERE — chronological feeds included, where it cannot sink a post but
    // must still hide it). The card cannot reach the manager, so the parent asks
    // `isMuted(post_id)` and folds in its session-local revealed set; `muted`
    // therefore means "render collapsed", not merely "matches a muted word".
    /** Render the collapse placeholder instead of the body. */
    muted?: boolean;
    /** One tap un-collapses THIS post for the session (the term stays muted). */
    onrevealmuted?: (post_id: string) => void;

    // ── Trained-topic training verbs (topic-factors.md § Training signals).
    /** The in-context factor — the composition's single `topic:*`, or null when
     *  none resolves (the verbs then open the target sheet, which the parent owns). */
    trainTarget?: string | null;
    /** This post's example marker for `trainTarget`: 'more' | 'less' | null. Comes
     *  from the sealed model, so it survives restarts and reaches every device. */
    trainMarker?: string | null;
    ontrain?: (post_id: string, verb: 'more' | 'less') => void;

    // ── Own-post delete (feed.md § State & data shape → Post deletion, IDs
    // user-approved 2026-07-16; mirrors conversations' dm-message-delete-button /
    // dm-message-delete-confirm-button two-step, and linux's
    // `build_post_actions_button` reference leg verbatim).
    /** Whether the local actor authored this post — gates `feed-post-delete-button`
     *  (the card can't reach `$identity` itself; the parent compares `post.author`). */
    isOwn?: boolean;
    ondelete?: (post_id: string) => void;

    // ── Content-policy render (family-safety.md § Content policy). The card
    // cannot reach the shared render-state, so the parent computes the verdict
    // (`contentVerdict(post.labels)`) and folds in its session-local reveal set,
    // exactly as with `muted`. The linux twins are `build_content_block` /
    // `build_content_collapse`.
    /** Render the block placeholder in place of the body — a guardian floor, no
     *  reveal, checked AHEAD of `muted` (absolute). */
    contentBlocked?: boolean;
    /** Render the collapse placeholder + one-tap reveal — a viewer's own
     *  threshold or a guardian `collapse` floor, checked AFTER `muted`. */
    contentCollapsed?: boolean;
    onrevealcontent?: (post_id: string) => void;
    /** The REGION's placeholder (`region-blocking.md` § The blocked render) —
     *  painted in place of the body AHEAD of every other arm, the family block
     *  included (tui's order). The parent passes it only while it applies: a
     *  `block` always, a `collapse` until the viewer reveals it (the reveal runs
     *  `onrevealcontent`, the same session-local set as the family collapse). */
    regionPlaceholder?: RegionPlaceholderValue | null;
    /** The viewer reported this post (or its author): the block notice names
     *  that act — "You reported this", `source="reported"` — and no body paints
     *  (`moderation.md` § Corollary). Only meaningful with `contentBlocked`. */
    contentReported?: boolean;
    /** `feed-post-report-button` in the ⋯ menu — gated `!isOwn`; the parent
     *  opens the shared report sheet on this post. */
    onreport?: (post_id: string) => void;

    // ── Own-post web-publishing verbs (web-content-hosting.md
    // § Published-post management; presence rules `ui/feed.md` § User
    // actions). State-derived off `post.web_slug`/`post.gated_tier`, which
    // the snapshot already carries — never a per-row query. Own posts only
    // (`isOwn`, already gating delete above).
    onpublishweb?: (post_id: string) => void;
    onunpublishweb?: (post_id: string) => void;
    oncopyweblink?: (post_id: string) => void;
    /** Whether the copy affordances have a serving origin to build on — the
     *  card can't reach the shared `siteLink` store itself; the parent
     *  resolves it once for every card (`web-content-hosting.md`: "legal but
     *  unreachable — the UI must say so" rather than a dead link). */
    webLinkOrigin?: string | null;
    oncopypaywalllink?: (post_id: string) => void;
    /** The last web/paywall link THIS post's verbs copied, so the confirming
     *  `copied` attr + line survive a re-render — parent-owned like every
     *  other cross-card-rebuild state here. */
    webLinkCopied?: { kind: 'web' | 'paywall'; url: string } | null;
  }

  let {
    post,
    decoded,
    blobUrl,
    mediaUrl,
    proxiedMediaUrl,
    playbackUrl,
    oninteract,
    contentLabel,
    onselect,
    onrevealremote,
    onbuyunlockoffer,
    onopenpaymentlink,
    muted = false,
    onrevealmuted,
    trainTarget = null,
    trainMarker = null,
    ontrain,
    isOwn = false,
    ondelete,
    contentBlocked = false,
    onpublishweb,
    onunpublishweb,
    oncopyweblink,
    webLinkOrigin = null,
    oncopypaywalllink,
    webLinkCopied = null,
    contentCollapsed = false,
    onrevealcontent,
    regionPlaceholder = null,
    contentReported = false,
    onreport,
  }: Props = $props();

  // The report verb: another's post only (`!is_own`).
  const showReport = $derived(!!onreport && !isOwn);

  function report(e: MouseEvent): void {
    e.stopPropagation();
    actionsOpen = false;
    onreport?.(post.post_id);
  }

  // The ⋯ overflow's open state is per-card (each card owns its own menu), and
  // the items live inside an `{#if}` — so a CLOSED menu has no items in the DOM
  // at all. That is deliberate and matches the dm-message-actions precedent (and
  // linux's popover, whose children are unmapped when closed): the e2e opens the
  // menu before reading a verb's state.
  let actionsOpen = $state(false);

  // The engagement-cue capture shell's probe key (`$lib/feed-cues`): only a card
  // that shows its post is an exposure. A region, block, mute or collapse
  // placeholder carries no subject, so lingering on one trains nothing.
  let cueSubject = $derived(!regionPlaceholder && !contentBlocked && !muted && !contentCollapsed);

  const trainState = (verb: 'more' | 'less'): 'on' | 'off' =>
    trainMarker === verb ? 'on' : 'off';

  function train(verb: 'more' | 'less', e: MouseEvent): void {
    e.stopPropagation(); // never let a menu click activate the card
    actionsOpen = false;
    ontrain?.(post.post_id, verb);
  }

  // Own-post delete: a destructive two-step inside the same flyout (first tap
  // reveals the confirm button, second tap fires). `deleteConfirming` resets
  // whenever the menu closes/reopens so a stray reopen never lands pre-armed.
  const showDelete = $derived(!!ondelete && isOwn);
  let deleteConfirming = $state(false);

  function requestDelete(e: MouseEvent): void {
    e.stopPropagation();
    deleteConfirming = true;
  }

  function confirmDelete(e: MouseEvent): void {
    e.stopPropagation();
    actionsOpen = false;
    deleteConfirming = false;
    ondelete?.(post.post_id);
  }

  // Own-post web-publishing verbs (web-content-hosting.md § Published-post
  // management). `showWebPublish` widens the ⋯ button's own opener condition
  // below — an own post with no train callback and (hypothetically) no
  // delete callback must still get an overflow button once it can publish.
  const showWebPublish = $derived(isOwn && !!onpublishweb);
  const webCopyDisabled = $derived(!webLinkOrigin);

  function publishWeb(e: MouseEvent): void {
    e.stopPropagation();
    actionsOpen = false;
    onpublishweb?.(post.post_id);
  }

  function unpublishWeb(e: MouseEvent): void {
    e.stopPropagation();
    actionsOpen = false;
    onunpublishweb?.(post.post_id);
  }

  function copyWebLink(e: MouseEvent): void {
    e.stopPropagation();
    if (!webLinkOrigin) return;
    oncopyweblink?.(post.post_id);
  }

  function copyPaywallLink(e: MouseEvent): void {
    e.stopPropagation();
    if (!webLinkOrigin) return;
    oncopypaywalllink?.(post.post_id);
  }

  // A REPOST ROW (feed.md § Interaction bar → Repost, ratified 2026-08-10):
  // attribution + the embedded original, no interaction bar of its own — the
  // repost post is empty by construction, so its own bar would be all zeros.
  let isRepostRow = $derived(!!post.reposted_post_id);

  // The quoted-post + media-image embeds, painted from the post `document`
  // (render-model.md § D6): the feed manager folds a `QuotedPost` / `Image` block
  // into `post.document` once `resolve_quoted_post` / `resolve_media` resolves, so
  // the card + media are sourced from the one document — no sibling `quotedPost`
  // prop, no `post.media_hash` read, and (since the typed `Video` variant landed) no
  // second app-side `decodePost` to tell an mp4 from a png: `mediaBlocks` is the
  // shared fold's own in-body-order answer, images and videos alike.
  let quotedBlock = $derived(quotedPostBlock(post.document));
  let mediaBlocks = $derived(documentMediaBlocks(post.document));

  // The resolved link-preview cards (render-model.md § D4): the feed manager folds
  // each bare-url `LinkPreview` block into `post.document` (Resolving) and, once
  // `resolve_link_preview` resolves it, projects the terminal state onto that block.
  // Paint a card per `Resolved` block (Resolving/Failed paint nothing — the inline
  // body link already shows); the feed page fires the resolve in `augmentPost`.
  // The `Resolved` match itself is the shared projection's, not web's.
  let previewCards = $derived(resolvedLinkPreviews(post.document));

  // The plain-post body as the shared `RenderDocument` (render-model.md § D6),
  // painted by the SAME `documentToHtml` walker the Conversations page uses — no
  // bespoke flat-text feed renderer. The full decoded body (web's rich
  // augmentation) supersedes the snapshot preview document once the per-post
  // decode lands; otherwise the snapshot's `post.document` (the preview) renders.
  //
  // Remote-image reveal is the MANAGER's (render-model.md § D3) for the snapshot doc, but the
  // fully-decoded body (web's rich augmentation) is a client-built `markdownToDocument` doc the
  // manager can't project onto — its blocks default `revealed: false` regardless of the manager's
  // per-post reveal set. So this card owns the SAME local-override toggle the post-detail view
  // does (`detailRevealed`, feed/+page.svelte): initialized false, flipped true by this card's own
  // reveal click (which ALSO still dispatches to the manager via `onrevealremote`, so quotedBlock /
  // mediaHash / previewCards above — all read straight off `post.document` — stay consistent, and
  // detail opened from an already-revealed card starts revealed too). Fixed 2026-08-01: before this,
  // the button was permanently a no-op once a post's body was decoded (true for every fauna/nostr
  // post — `augmentPost` in feed/+page.svelte), since neither the button's visibility nor the
  // painted `<img>` ever read anything BUT the never-revealed decoded doc — caught by widening
  // `test_feed_remote_image.py` off its tui-only origin.
  let cardRevealed = $state(false);
  let bodyDoc = $derived(decoded?.body ? markdownToDocument(decoded.body) : post.document);

  // One shared projection over the decoded body (`fauna_core::structured` via
  // wasm); each card branch reads its own discriminated variant.
  let structured = $derived(structuredView(decoded));
  let article = $derived(structured?.kind === 'article' ? structured : null);
  let community = $derived(structured?.kind === 'community' ? structured : null);
  let classified = $derived(structured?.kind === 'classified' ? structured : null);
  let liveActivity = $derived(structured?.kind === 'live-activity' ? structured : null);

  // Clicking the card opens the post detail — except when the click lands on an
  // interactive child (the InteractionBar buttons, a link, a media element), which
  // handles its own gesture.
  function handleCardActivate(e: Event): void {
    if (!onselect) return;
    if ((e.target as HTMLElement).closest('button, a, input, textarea, img')) return;
    onselect();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<article
  class="post-card"
  class:article-card={article !== null}
  class:community-card={community !== null}
  class:classified-card={classified !== null}
  class:live-activity-card={liveActivity !== null}
  class:selectable={onselect != null}
  data-testid={IDS.POST_CARD}
  data-post-id={post.post_id}
  data-cue-post={cueSubject ? post.post_id : undefined}
  data-cue-media={cueSubject ? String(!!post.has_media) : undefined}
  onclick={handleCardActivate}
  onkeydown={(e) => { if (e.key === 'Enter') handleCardActivate(e); }}
>
{#if regionPlaceholder}
  <RegionPlaceholder placeholder={regionPlaceholder} onreveal={() => onrevealcontent?.(post.post_id)} />
{:else if contentBlocked}
  <!-- Content-policy BLOCK (a guardian floor) — a policy-naming notice in place
       of the body, NO reveal (family-safety.md § Content policy; mirrors the
       legal-takedown tombstone). Checked AHEAD of `muted`, so a blocked post can
       never be revealed past the floor. `content-policy-blocked-notice` is the
       one ui.yaml ID this pillar renders (indexed, per post-card). -->
  <div class="content-policy-collapse">
    <span
      class="muted"
      data-testid={IDS.CONTENT_POLICY_BLOCKED_NOTICE}
      data-source={contentReported ? 'reported' : undefined}
    >{contentReported ? t.moderation.report.hidden_placeholder : t.family.content_blocked_notice}</span>
  </div>
{:else if muted}
  <!-- Muted-keyword collapse. The body, media and author are NOT rendered — the
       whole point is that the matched text never reaches the page (nor the
       accessibility tree). Revealing is per-post and session-local: the term
       stays muted, this just un-collapses this instance. The linux twin is
       `build_muted_collapse`; the conversation twin is `dm-message-muted`. -->
  <div class="muted-collapse">
    <span class="muted" data-testid={IDS.FEED_POST_MUTED}>{t.feed.post_muted_placeholder}</span>
    <button
      class="btn-secondary small"
      data-testid={IDS.FEED_POST_MUTED_REVEAL_BUTTON}
      onclick={(e) => { e.stopPropagation(); onrevealmuted?.(post.post_id); }}
    >{t.feed.post_muted_reveal}</button>
  </div>
{:else if contentCollapsed}
  <!-- Content-policy COLLAPSE (a viewer's own threshold, or a guardian
       `collapse` floor) — a one-tap reveal, session-local; the floor itself
       persists (the guardian relaxing it is what stops future collapse).
       Presentation only, no test id: v1 e2e drives the block case (linux
       precedent — `build_content_collapse`). -->
  <div class="content-policy-collapse">
    <span class="muted">{t.family.content_collapsed_notice}</span>
    <button
      class="btn-secondary small"
      onclick={(e) => { e.stopPropagation(); onrevealcontent?.(post.post_id); }}
    >{t.family.content_reveal_button}</button>
  </div>
{:else}
  <div class="post-header">
    <ProtocolBadge source={post.source} />
    <UnverifiedSourceBadge verification={post.verification} />
    <!-- The D10 audit marker (`delegated-origin-badge`): an external app wrote
         this post as the account. Beside the unverified badge, the order every
         app paints these two in. -->
    <DelegatedOriginBadge authoringOrigin={post.authoring_origin} />
    <span class="post-author" data-testid={IDS.POST_AUTHOR}>{shortActor(post.author)}</span>
    <!-- The attribution marker beside the author (id user-approved 2026-08-11):
         what lets an e2e tell a repost card from an empty-commentary quote card
         BY ELEMENT, instead of inferring it from the harness state dump. -->
    {#if isRepostRow}
      <span class="muted" data-testid={IDS.REPOST_ATTRIBUTION}>⇄ {t.feed.post.reposted_marker}</span>
    {/if}
    <!-- PostSummary.timestamp is already epoch-millis (the manager scaled the
         FeedPostItem micros down at the snapshot boundary). -->
    <span class="post-time muted">{relativeTime(post.timestamp)}</span>
    {#if contentLabel}
      <ContentLabelBadge label={contentLabel} />
    {/if}
    <!-- Gated-to-tier badge (feed.md § Encryption at rest; monetization.md § Pillars
         2+3): the tier name on every gated post's card. The list body stays the
         plaintext teaser; opening the detail unseals for an entitled reader. A room
         post this reader sits on the floor of names the room instead, by the
         reader's own label (PostSummary.room_label, derived by the shared manager;
         the composer's own "Room: ‹label›" string), and the card's detail-open is
         its "open"; a reader not in the room sees the reserved tier (feed.md
         § Encryption at rest → the app half, the card). -->
    {#if post.gated_tier}
      {#if post.room_label}
        <span
          class="gated-badge"
          data-testid={IDS.GATED_POST_BADGE}
          title={t.feed.post.gated_badge_room_tooltip({ room: post.room_label })}
        >{t.feed.post.gate_room({ room: post.room_label })}</span>
      {:else}
        <span
          class="gated-badge"
          data-testid={IDS.GATED_POST_BADGE}
          title={t.feed.post.gated_badge_tooltip({ tier: post.gated_tier })}
        >{post.gated_tier}</span>
      {/if}
    {/if}
    <!-- Buyer's price read (gap (2c), monetization.md § Per-post pay-to-unlock
         → the buyer's price read is post-addressed) — resolved lazily off
         fauna.subscriptions.post_unlock.get once gated_tier names a
         post-unlock-* tier (+page.svelte's augmentPost). Absent covers both
         "not yet resolved" and "the nest answered no offer": both leave the
         priceless teaser, with claim-code redemption (§5) as the fallback. -->
    {#if post.unlock_offer}
      <span class="muted" data-testid={IDS.GATED_POST_PRICE}>{post.unlock_offer.price_hint ?? ''}</span>
      {#if post.unlock_offer.payment_url}
        <button
          class="btn-secondary small"
          data-testid={IDS.GATED_POST_PAYMENT_LINK}
          onclick={(e) => { e.stopPropagation(); onopenpaymentlink?.(post.unlock_offer.payment_url); }}
        >{t.subscriptions.payment_url}</button>
      {/if}
      <button
        class="btn-primary small"
        data-testid={IDS.GATED_POST_BUY_BUTTON}
        onclick={(e) => { e.stopPropagation(); onbuyunlockoffer?.(post.post_id); }}
      >{t.feed.post.buy_button}</button>
    {/if}
    {#if ontrain || showDelete || showWebPublish || showReport}
      <!-- Per-card ⋯ overflow (the dm-message-actions-button precedent applied to
           posts). Hosts the trained-topic training verbs, own-post delete, and
           the own-post web-publishing verbs. -->
      <button
        class="actions-btn"
        data-testid={IDS.FEED_POST_ACTIONS_BUTTON}
        aria-haspopup="menu"
        aria-expanded={actionsOpen}
        onclick={(e) => {
          e.stopPropagation();
          actionsOpen = !actionsOpen;
          if (!actionsOpen) deleteConfirming = false;
        }}
      >⋯</button>
    {/if}
  </div>
  {#if actionsOpen}
    <!-- Inline (not an absolutely-positioned flyout) so every id attaches to a
         real element the Playwright driver counts — the dm-message-actions-menu
         shape. Items are absent from the DOM while closed, which is exactly the
         semantics the shared e2e expects (it opens the menu before reading a
         verb's state). -->
    <div class="actions-menu" data-testid={IDS.FEED_POST_ACTIONS_MENU}>
      {#if ontrain}
        <button
          class="menu-item"
          data-testid={IDS.FEED_POST_MORE_LIKE_THIS}
          data-state={trainState('more')}
          class:active={trainState('more') === 'on'}
          onclick={(e) => train('more', e)}
        >{t.feed.more_like_this}</button>
        <button
          class="menu-item"
          data-testid={IDS.FEED_POST_LESS_LIKE_THIS}
          data-state={trainState('less')}
          class:active={trainState('less') === 'on'}
          onclick={(e) => train('less', e)}
        >{t.feed.less_like_this}</button>
      {/if}
      {#if showWebPublish}
        <!-- The own-post web-publishing verbs (web-content-hosting.md
             § Published-post management). Deliberately OUTSIDE the training
             `{#if ontrain}` block above, which has no early-return here to
             hide behind (linux's build_web_publish_verbs precedent). -->
        {#if !post.web_slug}
          <!-- Unpublished: one verb, no link affordances for a page that
               does not exist. A default slug is the nest's to mint, so this
               needs no origin. -->
          <button
            class="menu-item"
            data-testid={IDS.FEED_POST_PUBLISH_WEB_BUTTON}
            onclick={publishWeb}
          >{t.web_publish.publish_to_web}</button>
        {:else}
          {#if post.gated_tier && webLinkOrigin}
            <p class="menu-note">{t.web_publish.paywall_link_note}</p>
          {/if}
          {#if !webLinkOrigin}
            <p class="menu-note">{t.web_publish.menu_no_link_reason}</p>
          {/if}
          <button
            class="menu-item"
            data-testid={IDS.FEED_POST_COPY_WEB_LINK_BUTTON}
            disabled={webCopyDisabled}
            data-copied={webLinkCopied?.kind === 'web' ? webLinkCopied.url : undefined}
            onclick={copyWebLink}
          >{t.web_publish.copy_web_link}</button>
          {#if post.gated_tier}
            <button
              class="menu-item"
              data-testid={IDS.FEED_POST_COPY_PAYWALL_LINK_BUTTON}
              disabled={webCopyDisabled}
              data-copied={webLinkCopied?.kind === 'paywall' ? webLinkCopied.url : undefined}
              onclick={copyPaywallLink}
            >{t.web_publish.copy_paywall_link}</button>
          {/if}
          <button
            class="menu-item"
            data-testid={IDS.FEED_POST_UNPUBLISH_WEB_BUTTON}
            onclick={unpublishWeb}
          >{t.web_publish.unpublish}</button>
          {#if webLinkCopied}
            <p class="menu-note">
              {webLinkCopied.kind === 'web'
                ? t.web_publish.copied_link({ url: webLinkCopied.url })
                : t.web_publish.copied_paywall_link({ url: webLinkCopied.url })}
            </p>
          {/if}
        {/if}
      {/if}
      {#if showReport}
        <button
          class="menu-item"
          data-testid={IDS.FEED_POST_REPORT_BUTTON}
          onclick={report}
        >{t.feed.report_post}</button>
      {/if}
      {#if showDelete}
        <!-- Destructive two-step, mirroring dm-message-delete-button /
             dm-message-delete-confirm-button and linux's
             build_post_actions_button reference leg: first tap reveals the
             confirm button in place, second tap fires. -->
        {#if !deleteConfirming}
          <button
            class="menu-item delete-option"
            data-testid={IDS.FEED_POST_DELETE_BUTTON}
            onclick={requestDelete}
          >{t.feed.delete_post}</button>
        {:else}
          <button
            class="menu-item delete-option"
            data-testid={IDS.FEED_POST_DELETE_CONFIRM_BUTTON}
            onclick={confirmDelete}
          >{t.feed.delete_post_confirm}</button>
        {/if}
      {/if}
    </div>
  {/if}
  <div class="post-body">
    {#if article !== null}
      {#if article.image}
        <img class="article-hero" src={article.image} alt={article.title} loading="lazy" />
      {/if}
      {#if article.title}
        <h3 class="article-title" data-testid={IDS.FEED_POST_TEXT}>{article.title}</h3>
      {/if}
      {#if article.summary}
        <p class="article-summary muted">{article.summary}</p>
      {/if}
      {#if article.content}
        <div class="article-body">
          {@html markdownToHtml(article.content)}
        </div>
      {/if}
    {:else if community !== null}
      <div class="community-card-inner">
        <span class="content-type-label">{t.feed.post_type.community}</span>
        <h3 class="community-name" data-testid={IDS.FEED_POST_TEXT}>{community.name}</h3>
        {#if community.description}
          <p class="community-description muted">{community.description}</p>
        {/if}
        {#if community.rules}
          <p class="community-rules">{community.rules}</p>
        {/if}
      </div>
    {:else if classified !== null}
      <div class="classified-card-inner">
        <span class="content-type-label">{t.feed.post_type.classified}</span>
        <h3 class="classified-title" data-testid={IDS.FEED_POST_TEXT}>{classified.title}</h3>
        <div class="classified-meta">
          {#if classified.price}
            <span class="price-badge">{classified.price}</span>
          {/if}
          {#if classified.location}
            <span class="classified-location muted">📍 {classified.location}</span>
          {/if}
          {#if classified.condition}
            <span class="classified-condition muted">{classified.condition}</span>
          {/if}
        </div>
        {#if classified.content}
          <p class="classified-description">{classified.content}</p>
        {/if}
      </div>
    {:else if liveActivity !== null}
      <div class="live-activity-card-inner">
        <div class="live-activity-header">
          {#if liveActivity.title}
            <h3 class="live-activity-title" data-testid={IDS.FEED_POST_TEXT}>{liveActivity.title}</h3>
          {/if}
          <span class="status-badge {liveStatusClass(liveActivity.status)}">{liveActivity.status}</span>
        </div>
        {#if liveActivity.summary}
          <p class="live-activity-summary muted">{liveActivity.summary}</p>
        {/if}
        <div class="live-activity-meta">
          {#if liveActivity.streaming_url}
            <a class="watch-link" href={liveActivity.streaming_url} target="_blank" rel="noopener noreferrer">{t.feed.watch}</a>
          {/if}
          {#if liveActivity.participants}
            <span class="participant-count muted">👥 {liveActivity.participants}</span>
          {/if}
        </div>
      </div>
    {:else}
      <!-- Plain post. The body is painted through the shared `RenderDocument`
           walker (`documentToHtml`, the same one the Conversations page uses —
           render-model.md § D6), so markdown structure renders uniformly instead
           of as flat text. Placeholder only when the body is empty. -->
      {#if (bodyDoc?.blocks?.length ?? 0) > 0}
        <div class="post-content" data-testid={IDS.FEED_POST_TEXT}>{@html documentToHtml(bodyDoc, { revealed: cardRevealed })}</div>
        {#if !cardRevealed && documentHasBlockedRemoteImages(bodyDoc)}
          <button
            data-testid={IDS.LOAD_REMOTE_CONTENT_BUTTON}
            class="load-remote-btn"
            onclick={() => { cardRevealed = true; onrevealremote?.(post.post_id); }}
          >{t.conversations.detail.load_remote_content}</button>
        {/if}
      {:else}
        <p class="muted post-id-line">{post.post_id.slice(0, 16)}…</p>
      {/if}
    {/if}
    <!-- Tag chips from the snapshot facet list (feed.md § Layout — the
         PostSummary.tags list, not parsed out of the body). -->
    {#if post.tags && post.tags.length > 0}
      <div class="post-tags">
        {#each post.tags as tag}
          <span data-testid={IDS.TAG_CHIP} class="tag-chip">#{tag}</span>
        {/each}
      </div>
    {/if}
    <!-- Media: every trusted media block the SHARED fold emitted, in body order
         (render-model.md § D6 — FeedManager::resolve_media folds one typed block per
         MediaItem). Web keeps its multi-item render, but no longer derives it from a
         second app-side `decodePost`: the image-vs-video branch is now made once in
         shared Rust, which is what makes `video-thumbnail` reachable on all 7 apps
         instead of only here. -->
    {#if mediaBlocks.length > 0}
      <div class="post-media">
        {#each mediaBlocks as block}
          {#if 'Image' in block}
            <!-- `mediaUrl`, not `blobUrl`: a tier-restricted post's attachment is
                 sealed under the post's own per-post key and no URL can render it,
                 so the page hands back a local object URL for those and the plain
                 blob URL for everything else. No is-this-post-gated branch here —
                 the card paints whatever string it is given (media.md
                 § Encryption at rest). `null` = in flight or would not open. -->
            {@const mediaSrc = mediaUrl(block.Image.hash)}
            {#if mediaSrc}
              <C2paImage data-testid={IDS.POST_IMAGE} data-blob-hash={block.Image.hash} src={mediaSrc} alt={block.Image.alt || 'post media'} class="post-media-img" thumb />
            {/if}
          {:else if 'Video' in block}
            <VideoThumbnail
              src={blobUrl(block.Video.hash)}
              alt={block.Video.alt || 'post video'}
              resolveSource={playbackUrl ? () => playbackUrl(block) : undefined}
            />
          {:else if 'ProxiedImage' in block}
            <!-- A bridged post's own picture (render-model.md § D6c): an authenticated fetch of
                 its nest-relative path → object URL, never a bare `<img src>` to the path. -->
            <ProxiedImage
              path={block.ProxiedImage.path}
              alt={block.ProxiedImage.alt}
              src={proxiedMediaUrl ? proxiedMediaUrl(block.ProxiedImage.path) : null}
              class="post-media-img"
            />
          {:else if 'ProxiedVideo' in block}
            <!-- A bridged post's own video (render-model.md § D6c → *Proxied video*): the play
                 glyph over a poster-less frame carrying the path — never byte-loaded, and inert
                 until the shared playback projection answers for it. -->
            <VideoThumbnail
              caption={block.ProxiedVideo.path}
              alt={block.ProxiedVideo.alt || 'post video'}
            />
          {/if}
        {/each}
      </div>
    {/if}
    <!-- Quoted-post embed, painted from the folded document `QuotedPost` block
         (render-model.md § D6 — FeedManager::resolve_quoted_post folds it in), in
         both list card + post_detail (feed.md § Layout). -->
    {#if quotedBlock}
      <QuotedPost post_id={quotedBlock.post_id} view={quotedBlock} />
    {/if}
    <!-- Link-preview cards, one per Resolved `LinkPreview` block in the document
         (render-model.md § D4 — FeedManager::resolve_link_preview resolves them; the
         feed page fires the resolve in augmentPost). -->
    {#each previewCards as lp (lp.url)}
      <LinkPreviewCard
        url={lp.url}
        title={lp.title}
        description={lp.description}
        imageUrl={lp.revealed && lp.image_hash ? blobUrl(lp.image_hash) : null}
      />
    {/each}
    <!-- The post tip surface (`monetization.md` § Tips), behind the web family's
         `payments` compile condition — see `TipSurface.svelte`'s header for why
         the gate must sit on the RENDER even though `post.tips` is already
         `null` in an excised build. The vite define folds this to `false` in the
         store-safe flavor and the import goes with it, so neither the chunk nor
         any `post-tip-*` id reaches that bundle. -->
    {#if __FAUNA_PAYMENTS__}
      <TipSurface tips={post.tips} />
    {/if}
  </div>
  {#if !isRepostRow}
    <InteractionBar
      post_id={post.post_id}
      like_count={post.like_count}
      reply_count={post.reply_count}
      repost_count={post.repost_count}
      quote_count={post.quote_count}
      viewer_liked={post.viewer_liked}
      viewer_reposted={!!post.viewer_repost_id}
      {oninteract}
    />
  {/if}
{/if}
</article>

<style>
  .muted-collapse { display: flex; align-items: center; gap: 0.75rem; }
  .actions-btn {
    margin-left: auto;
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.25rem;
  }
  .actions-menu {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.125rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    padding: 0.25rem;
    margin: 0.25rem 0;
  }
  .menu-item {
    background: none;
    border: none;
    color: var(--text);
    text-align: left;
    padding: 0.375rem 0.5rem;
    border-radius: 4px;
    cursor: pointer;
    font-size: 0.8125rem;
  }
  .menu-item:hover { background: var(--bg-hover); }
  .menu-item.active { background: var(--bg-hover); font-weight: 600; }
  .menu-item.delete-option { color: var(--danger, #e74c3c); }
  .menu-item.delete-option:hover { background: rgba(231, 76, 60, 0.1); }
  .menu-note {
    margin: 0.125rem 0.5rem;
    padding: 0;
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  /* The tip surface's styles moved with its markup into
     `payments/TipSurface.svelte` — leaving them here would keep the class names
     in a store-safe bundle, and Svelte would warn them unused besides. */
  .post-card {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 0.75rem;
    margin-bottom: 0.75rem;
    background: var(--bg-card, var(--bg));
  }
  .post-header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.5rem;
    font-size: 0.8rem;
  }
  .post-card.selectable { cursor: pointer; }
  .post-author { font-weight: 600; }
  .post-time { margin-left: auto; }
  .post-body { font-size: 0.85rem; line-height: 1.5; }
  .post-content { margin: 0; white-space: pre-wrap; }
  .muted { color: var(--text-muted); }

  .post-tags { display: flex; flex-wrap: wrap; gap: 0.25rem; margin-top: 0.5rem; }
  .tag-chip {
    font-size: 0.7rem;
    padding: 0.1rem 0.4rem;
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--accent);
  }

  .post-media { margin-top: 0.5rem; }
  .post-id-line { font-size: 0.75rem; margin: 0; }

  /* Article variant */
  .article-hero { width: 100%; max-height: 300px; object-fit: cover; border-radius: 4px; margin-bottom: 0.5rem; }
  .article-title { margin: 0 0 0.25rem; font-size: 1rem; }
  .article-summary { margin: 0 0 0.5rem; font-size: 0.8rem; }
  .article-body { font-size: 0.85rem; line-height: 1.6; }

  /* Community variant */
  .content-type-label {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
    font-weight: 600;
  }
  .community-name { margin: 0.25rem 0; font-size: 1rem; }
  .community-description { margin: 0 0 0.25rem; font-size: 0.8rem; }
  .community-rules { font-size: 0.8rem; margin: 0; }

  /* Classified variant */
  .classified-title { margin: 0.25rem 0; font-size: 1rem; }
  .classified-meta { display: flex; gap: 0.5rem; align-items: center; margin-bottom: 0.25rem; font-size: 0.8rem; }
  .price-badge { background: var(--accent); color: #fff; padding: 0.1rem 0.4rem; border-radius: 4px; font-weight: 600; font-size: 0.75rem; }
  .classified-description { font-size: 0.85rem; margin: 0.25rem 0 0; }

  /* Live activity variant */
  .live-activity-header { display: flex; align-items: center; gap: 0.5rem; }
  .live-activity-title { margin: 0; font-size: 1rem; }
  .status-badge { font-size: 0.65rem; padding: 0.1rem 0.4rem; border-radius: 4px; font-weight: 600; text-transform: uppercase; }
  .status-live { background: #e74c3c; color: #fff; }
  .status-ended { background: var(--text-muted); color: #fff; }
  .status-planned { background: var(--accent); color: #fff; }
  .live-activity-summary { margin: 0.25rem 0; font-size: 0.8rem; }
  .live-activity-meta { display: flex; gap: 0.5rem; align-items: center; font-size: 0.8rem; }
  .watch-link { color: var(--accent); font-weight: 600; text-decoration: none; }

  /* Gated-to-tier badge (feed.md § Encryption at rest) — the tier name on a gated card. */
  .gated-badge { background: var(--accent); color: #fff; padding: 0.1rem 0.4rem; border-radius: 4px; font-weight: 600; font-size: 0.7rem; }
</style>
