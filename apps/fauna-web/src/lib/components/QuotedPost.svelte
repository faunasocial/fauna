<script lang="ts">
  import { shortActor } from '$lib/feed-utils';
  import { legalTakedownTombstone } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import UnverifiedSourceBadge from './UnverifiedSourceBadge.svelte';
  import DelegatedOriginBadge from './DelegatedOriginBadge.svelte';
  import { IDS } from '$lib/generated/uiIds';
  import { t } from '$lib/i18n/strings';

  /** The shared `fauna_feed::QuotedPostView` — `{ post_id, author, body, verification,
   *  legal_takedown_ref }` with `body` already truncated to the 280-char cap by the
   *  manager's `resolve_quoted_post` projection (`feed.md` § Post content types). `null`
   *  while the in-page projection / `fauna.posts.get` fallback resolves. When the
   *  quoted post's signed envelope failed THIS client's verification
   *  (`view.verification === 'Failed'`; security.md § Client display of unverified
   *  content) the header also paints the `unverified-source-badge`, scoped under
   *  this card's `quoted-post` id (Slice 2b) — and, one field over, the
   *  `delegated-origin-badge` when the *quoted* post was authored by an external
   *  app through the D10 delegated sub-key (`view.authoring_origin === 'Delegated'`;
   *  atproto-pds-full.md § D10 → *Audit*), keyed off the quoted post's own origin
   *  and independent of the focal post's. When the quoted post has been **taken
   *  down under a legal obligation** (`view.legal_takedown_ref` set; moderation.md §
   *  Categories & enforcement item 1) the card renders the shared tombstone in place
   *  of the withheld body — never a blank/broken embed. When the quoted post is **no
   *  longer there** (`view.not_found`: its author deleted it; `feed.md` § Post
   *  deletion — references dangle by design) the card paints `feed.post.post_not_found`
   *  in the same place, instead of an embed with an empty author and body. */
  interface Props {
    post_id: string;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    view: any | null;
  }

  let { post_id, view }: Props = $props();
</script>

<div class="quoted-post" data-testid={IDS.QUOTED_POST}>
  {#if view?.legal_takedown_ref}
    <p class="quoted-body quoted-legal-takedown">
      {resolveLocalized(legalTakedownTombstone(view.legal_takedown_ref))}
    </p>
  {:else if view?.not_found}
    <p class="quoted-body quoted-not-found">{t.feed.post.post_not_found}</p>
  {:else if view}
    <div class="quoted-header">
      <span class="quoted-author">{shortActor(view.author)}</span>
      <UnverifiedSourceBadge verification={view.verification} />
      <DelegatedOriginBadge authoringOrigin={view.authoring_origin} />
    </div>
    <p class="quoted-body">{view.body}</p>
  {:else}
    <p class="muted quoted-loading">{post_id.slice(0, 16)}...</p>
  {/if}
</div>

<style>
  .quoted-post {
    border-left: 3px solid var(--border);
    padding: 0.5rem 0.75rem;
    margin-top: 0.5rem;
    background: var(--bg-surface);
    border-radius: 4px;
  }
  .quoted-header { margin-bottom: 0.25rem; }
  .quoted-author { font-size: 0.75rem; font-weight: 600; color: var(--text-muted); }
  .quoted-body { font-size: 0.8rem; margin: 0; }
  .quoted-legal-takedown,
  .quoted-not-found { color: var(--text-muted); font-style: italic; }
  .quoted-loading { font-size: 0.75rem; margin: 0; }
  .muted { color: var(--text-muted); }
</style>
