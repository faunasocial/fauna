// Shared, REACTIVE state for the web-publish origin resolution
// (web-content-hosting.md § Published-post management), read by BOTH the
// `web-settings` Published-posts section (`WebSettingsSection.svelte`) and
// the feed ⋯-overflow verbs (`routes/feed/+page.svelte` + `PostCard.svelte`),
// so the two surfaces never disagree about a creator's address — the
// linux/tui `cached_site_link` shape, adapted to a Svelte store since these
// are separate route components with no shared component state, and the
// feed's cards must re-render once a lazy hydrate resolves.
import { derived, get, writable } from 'svelte/store';
import { identity } from './store';
import { registerActorScopedReset, sameActorSince } from './actorScope';
import { webDomainGet, webGetSubdomainEnabled, webPublishedSite, webServingDomain } from './rpc';
import { webSiteLinkView, type SiteLinkView } from './wasm';

export interface PublishedPost {
  post_id: number[];
  slug: string;
  gated_tier?: string;
}

export interface WebPage {
  enabled: boolean;
  servingDomain: string;
  domains: Array<{ domain: string; status: string }>;
  posts: PublishedPost[];
  /** The nest blanked the rendered pages and has not restored them yet
   *  (`web-settings-render-status`). */
  renderedPagesDown: boolean;
}

/** `null` until either surface has read it once this session.
 *  `WebSettingsSection` always overwrites it with its own fresh read on
 *  mount — a visit there is the authoritative re-hydrate. */
export const webPage = writable<WebPage | null>(null);

/** The origin every copy affordance builds on, resolved from `webPage` +
 *  the identity store — active custom domain beats an enabled subdomain
 *  (`fauna_client_web::site_link_view`). `null` until `webPage` lands. */
export const siteLink = derived<[typeof webPage, typeof identity], SiteLinkView | null>(
  [webPage, identity],
  ([$webPage, $identity]) =>
    $webPage
      ? webSiteLinkView(
          $webPage.domains,
          $webPage.enabled,
          $identity?.handle ?? null,
          $webPage.servingDomain,
        )
      : null,
);

/** Clear the cache on an actor switch (`registerActorScopedReset`,
 *  `$lib/actorScope`) — a bare module-level store survives a switch
 *  otherwise, and the next actor would see the FIRST actor's published posts
 *  and origin until their own visit to Settings → Web overwrote it.
 *
 *  Also drops the in-flight [`hydrating`] promise: left standing, the
 *  incoming actor's first [`ensureWebPageHydrated`] call would join the
 *  OUTGOING actor's already-dropped read instead of starting its own —
 *  silently, since nothing else clears it. */
export function resetWebPage(): void {
  webPage.set(null);
  hydrating = null;
}
registerActorScopedReset(resetWebPage);

/** Best-effort clipboard write shared by both surfaces' copy affordances.
 *  `navigator.clipboard` can be absent or permission-blocked in a headless/
 *  automated browser — `.writeText` access itself then throws SYNCHRONOUSLY
 *  (not merely a rejected promise), which an unwrapped `x().catch()` does not
 *  catch. The real OS clipboard write is best-effort UX sugar; callers must
 *  still record their own `copied` confirmation state regardless of whether
 *  this succeeds — that state, not the OS clipboard, is what the e2e driver
 *  reads (`web-published-post-copy-link-button`'s `copied` attr). */
export async function bestEffortCopyToClipboard(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    /* headless / no perm */
  }
}

let hydrating: Promise<WebPage> | null = null;

/** Ensure `webPage` is populated, for a caller (the feed ⋯-menu) that
 *  reaches the web-publishing verbs without ever having opened Settings →
 *  Web. A no-op once populated; concurrent callers share the one in-flight
 *  read. Setting the store is what makes every subscribed card re-render
 *  once the read lands. */
export async function ensureWebPageHydrated(): Promise<WebPage> {
  const current = get(webPage);
  if (current) return current;
  if (!hydrating) {
    const id = get(identity);
    const secretHex = id?.secretHex ?? '';
    // The identity seam ahead of the write below (`actorScope.ts`): a switch
    // landing mid-build is not stopped by `resetWebPage`, which only nulls
    // `webPage`/`hydrating` — this build resolves whatever the switch did and
    // would assign the DEPARTING actor's origin and published posts, restoring
    // exactly the wrong-actor render (`account-scoping.md` § The scoping
    // taxonomy → the in-memory corollary).
    const stillThisActor = sameActorSince();
    hydrating = (async () => {
      const servingDomain = await webServingDomain(secretHex);
      const enabled = await webGetSubdomainEnabled(secretHex);
      const domains = await webDomainGet(secretHex);
      const site = await webPublishedSite(secretHex);
      if (!stillThisActor()) {
        throw new Error('web page: actor changed while hydrating');
      }
      const page: WebPage = {
        enabled,
        servingDomain,
        domains,
        posts: site.posts,
        renderedPagesDown: site.rendered_pages_down,
      };
      webPage.set(page);
      return page;
    })();
  }
  try {
    return await hydrating;
  } finally {
    hydrating = null;
  }
}
