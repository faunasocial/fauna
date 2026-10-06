<script lang="ts">
  // User-settings "Web" section (`web-settings`): the per-user authoring surface
  // for web-content hosting (web-content-hosting.md § Published-post
  // management). One control — the **subdomain opt-in toggle**
  // (`web-settings-subdomain-toggle`, default OFF) that serves the user's `web`
  // content at `https://<handle>.<domain>/` — with the live URL (or a disabled
  // reason) below it and a static content explainer. A dumb renderer of the
  // shared `WebClient` over the singleton WS-RPC client (the `webGet/SetSubdomainEnabled`
  // calls in `$lib/rpc`, the WASM twin of the linux native `WebClient` +
  // `fauna-ffi`'s `FfiWebClient`); the URL hint is the shared `webSubdomainView`
  // projection (`fauna_client_web::subdomain_view`), so the reserved-label rule +
  // the `<handle>.<domain>` URL never drift from the nest's routing (priority #2).
  // Lifts the linux lead shape (apps/fauna-linux/src/settings/web.rs). The admin
  // uses this same page for their own site; the nest-wide apex designation is the
  // separate `admin-web` page. Reuses the settings page's single `error-message`
  // (no duplicate IDs) via the bindable `error`.
  // Behavior + IDs: docs/goal/behavior/web-content-hosting.md, ui.yaml § web-settings.
  import { identity } from '$lib/store';
  import {
    webDomainGet,
    webGetSubdomainEnabled,
    webPaywallMintToken,
    webPublishedSite,
    webPublishUnset,
    webServingDomain,
    webSetSubdomainEnabled,
  } from '$lib/rpc';
  import {
    ensureWasm,
    webDisabledReasonText,
    webPostPageUrl,
    webSubdomainView,
    webTokenedUrl,
    type SiteLinkDisabledReason,
    type SubdomainView,
  } from '$lib/wasm';
  import { bestEffortCopyToClipboard, webPage, siteLink, type PublishedPost } from '$lib/web-publish';
  import { onMount } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import { t } from '$lib/i18n/strings';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface; web dispatch errors
  // flow into it.
  let { error = $bindable('') } = $props();

  // The current opt-in + its projected view. `enabled` (and the `data-state` attr
  // the e2e reads) is set ONLY from a nest-confirmed read/echo (non-optimistic),
  // so reading it back proves the round-trip.
  let enabled = $state(false);
  let view = $state<SubdomainView | null>(null);

  // The host the NEST serves web content on, read from `fauna.nest.info` at
  // mount. NOT the identity store's cached sign-in `domain`, which this page
  // used to pass: that value is the `"localhost"` placeholder on a domainless
  // nest, and `<handle>.localhost` is precisely the host the nest's resolver
  // never strips — so the row advertised a URL that cannot load. The cache
  // is not consulted: `servingDomain` comes from nest.info (web-content-hosting.md
  // § Published-post management).
  let servingDomain = $state('');

  // ── Published-posts management section (web-content-hosting.md
  //    § Published-post management). `posts` + the origin resolution
  //    (`$siteLink`, `$lib/web-publish`) are the SAME shared store the feed
  //    ⋯-menu reads, so the two surfaces can never disagree. `hydrated` is the
  //    "nobody has read yet" gate tui/linux use, so a pre-read frame never
  //    claims "no published posts" about a list nobody asked for. ──
  let posts = $state<PublishedPost[]>([]);
  let hydrated = $state(false);
  // The nest blanked the rendered pages and has not restored them yet. Painted
  // only off a read that said so (`web-settings-render-status`): information
  // only — the nest restores the site by itself.
  let renderedPagesDown = $state(false);
  let copied = $state<{ postId: number[]; kind: 'web' | 'paywall'; url: string } | null>(null);

  // Project the toggle view from the opt-in flag + the actor's handle and that
  // serving domain. Pure shared Rust.
  function applyView(on: boolean): void {
    enabled = on;
    view = webSubdomainView(on, $identity?.handle ?? null, servingDomain);
  }

  // Read the full `web-settings` page and render both surfaces from one
  // answer. Runs at mount AND on every later re-entry to this sub-page
  // (`afterNavigate` below) — the Published-posts list is observer-free, so
  // a re-visit must re-read `publish.list` or a post published from the feed
  // ⋯-menu, or from another device, never appears.
  //
  // ⚠ A SvelteKit `goto()` to the route this component is ALREADY mounted on
  // does not remount it — the parent's `{:else if current === 'web'}` branch
  // stays satisfied, so `onMount` alone only ever fires once per settings
  // visit. `afterNavigate` is the reliable per-navigation-EVENT signal
  // (fires even when the destination route is unchanged), unlike `$derived`/
  // `$effect` over `$page.params`, whose tracked VALUE does not change on a
  // same-route re-navigation. Mirrors linux's `set_visible_child_forced` fix
  // for the identical GTK-side gap (`views/nav_rail.rs`).
  async function hydrate(): Promise<void> {
    try {
      await ensureWasm();
      const secretHex = $identity?.secretHex ?? '';
      // `call()` waits for the socket, so no manual connect-retry is needed.
      servingDomain = await webServingDomain(secretHex);
      const [subEnabled, domainRows, site] = await Promise.all([
        webGetSubdomainEnabled(secretHex),
        webDomainGet(secretHex),
        webPublishedSite(secretHex),
      ]);
      applyView(subEnabled);
      posts = site.posts;
      renderedPagesDown = site.rendered_pages_down;
      hydrated = true;
      webPage.set({
        enabled: subEnabled,
        servingDomain,
        domains: domainRows,
        posts: site.posts,
        renderedPagesDown: site.rendered_pages_down,
      });
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(() => {
    void hydrate();
  });
  afterNavigate(({ to }) => {
    if (to?.route.id?.endsWith('/settings/[[subpage]]')) void hydrate();
  });

  // Flip the opt-in, then re-render from the nest's echoed state (non-optimistic).
  async function onToggle(): Promise<void> {
    error = '';
    try {
      applyView(await webSetSubdomainEnabled($identity?.secretHex ?? '', !enabled));
      webPage.update((p) => (p ? { ...p, enabled } : p));
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  // The URL row text: the live URL (whether on or off — it's where the site would
  // serve), else the disabled reason.
  let urlText = $derived(
    view?.url
      ? view.url
      : view?.disabled_reason === 'NoHandle'
        ? t.web_settings.subdomain_no_handle
        : view?.disabled_reason === 'ReservedLabel'
          ? t.web_settings.subdomain_reserved
          : view?.disabled_reason === 'NoServingDomain'
            ? t.web_settings.subdomain_no_serving_domain
            : '',
  );

  // The `disabled_reason` text beside a dead copy affordance — the settings
  // page's own richer reasons (unlike the feed ⋯-menu, which cannot point
  // "above" and uses one generic line instead). The key mapping is the shared
  // `fauna_client_web::disabled_reason_text` door (web-content-hosting.md
  // § Published-post management) — tui/linux/macOS/iOS resolve the same door.
  function linkDisabledText(reason: SiteLinkDisabledReason | null | undefined): string {
    return resolveLocalized(webDisabledReasonText(reason ?? null));
  }

  function samePost(a: number[], b: number[]): boolean {
    return a.length === b.length && a.every((v, i) => v === b[i]);
  }

  async function copyWebLink(post: PublishedPost): Promise<void> {
    const origin = $siteLink?.origin;
    if (!origin) return;
    const url = webPostPageUrl(origin, post.slug);
    await bestEffortCopyToClipboard(url);
    copied = { postId: post.post_id, kind: 'web', url };
  }

  async function copyPaywallLink(post: PublishedPost): Promise<void> {
    error = '';
    try {
      const minted = await webPaywallMintToken($identity?.secretHex ?? '', post.slug);
      const origin = $siteLink?.origin;
      if (!origin) return;
      const url = webTokenedUrl(origin, minted.path, minted.token);
      await bestEffortCopyToClipboard(url);
      copied = { postId: post.post_id, kind: 'paywall', url };
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      error = t.web_publish.error_paywall_link({ message: msg });
    }
  }

  // Take a published post down, then re-read `publish.list` so the section
  // reflects the nest's state — a takedown that half-applied then shows up as
  // a row that stayed rather than one that vanished from a screen the nest
  // disagrees with (mirrors linux's `unpublish`).
  async function unpublish(post: PublishedPost): Promise<void> {
    error = '';
    try {
      const secretHex = $identity?.secretHex ?? '';
      await webPublishUnset(secretHex, post.post_id);
      const site = await webPublishedSite(secretHex);
      posts = site.posts;
      renderedPagesDown = site.rendered_pages_down;
      if (copied && samePost(copied.postId, post.post_id)) copied = null;
      webPage.update((p) =>
        p ? { ...p, posts: site.posts, renderedPagesDown: site.rendered_pages_down } : p,
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      error = t.web_publish.error_unpublish({ message: msg });
    }
  }
</script>

<section class="section" data-testid={IDS.WEB_SETTINGS}>
  <h2>{t.web_settings.title}</h2>

  <!-- Subdomain opt-in toggle (default OFF). `data-state` (the e2e read idiom)
       tracks the nest, not an optimistic local guess. -->
  <div class="toggle-row">
    <button
      class="btn toggle"
      class:on={enabled}
      data-testid={IDS.WEB_SETTINGS_SUBDOMAIN_TOGGLE}
      data-state={enabled ? 'on' : 'off'}
      onclick={onToggle}
    >{t.web_settings.subdomain_toggle_label}</button>
    <p class="muted small">{t.web_settings.subdomain_toggle_subtitle}</p>
  </div>

  <!-- Live URL / disabled-reason row. -->
  <div class="url-row">
    <span class="caption">{t.web_settings.subdomain_url_label}</span>
    <span class="url" data-testid={IDS.WEB_SETTINGS_SUBDOMAIN_URL}>{urlText}</span>
  </div>

  <!-- The blanked-site status line: information only, no button — the nest
       restores the site by itself (web-content-hosting.md § Routing, render,
       serving → *A blanked site tells its author*). Absent on a healthy site. -->
  {#if renderedPagesDown}
    <p class="muted small" data-testid={IDS.WEB_SETTINGS_RENDER_STATUS}>
      {t.web_settings.render_status_down}
    </p>
  {/if}

  <!-- Static content explainer. -->
  <p class="muted small" data-testid={IDS.WEB_SETTINGS_CONTENT_INFO}>{t.web_settings.content_info}</p>

  <!-- Published-posts management section — painted only once the page has
       actually read the list (`hydrated`); a pre-read frame must not claim
       "no published posts" about a list nobody asked for. -->
  {#if hydrated}
    <h2 class="section-title">{t.web_settings.published_posts_title}</h2>
    <div data-testid={IDS.WEB_PUBLISHED_POSTS_LIST}>
      {#if posts.length === 0}
        <p class="muted small" data-testid={IDS.WEB_PUBLISHED_POSTS_EMPTY}>
          {t.web_settings.published_posts_empty}
        </p>
      {:else}
        {#if posts.some((p) => p.gated_tier)}
          <p class="muted small">{t.web_publish.paywall_link_note}</p>
        {/if}
        {#if !$siteLink?.origin}
          <p class="muted small">{linkDisabledText($siteLink?.disabled_reason)}</p>
        {/if}
        {#each posts as post (post.slug)}
          <div
            class="published-row"
            data-testid={IDS.WEB_PUBLISHED_POST_ITEM}
            data-gated-tier={post.gated_tier ?? ''}
          >
            <span class="slug" data-testid={IDS.WEB_PUBLISHED_POST_SLUG}>{post.slug}</span>
            {#if post.gated_tier}
              <span class="muted small">{t.web_settings.published_post_gated_badge({ tier: post.gated_tier })}</span>
            {/if}
            <button
              class="btn-secondary small"
              data-testid={IDS.WEB_PUBLISHED_POST_COPY_LINK_BUTTON}
              disabled={!$siteLink?.origin}
              data-copied={copied && samePost(copied.postId, post.post_id) && copied.kind === 'web'
                ? copied.url
                : undefined}
              onclick={() => copyWebLink(post)}
            >{t.web_publish.copy_web_link}</button>
            {#if post.gated_tier}
              <button
                class="btn-secondary small"
                data-testid={IDS.WEB_PUBLISHED_POST_COPY_PAYWALL_LINK_BUTTON}
                disabled={!$siteLink?.origin}
                data-copied={copied && samePost(copied.postId, post.post_id) && copied.kind === 'paywall'
                  ? copied.url
                  : undefined}
                onclick={() => copyPaywallLink(post)}
              >{t.web_publish.copy_paywall_link}</button>
            {/if}
            <button
              class="btn-secondary small"
              data-testid={IDS.WEB_PUBLISHED_POST_UNPUBLISH_BUTTON}
              onclick={() => unpublish(post)}
            >{t.web_publish.unpublish}</button>
          </div>
        {/each}
        <!-- The copied value, painted back under the list — the devices-page
             lesson: an unasserted copy affordance rots invisibly. -->
        {#if copied}
          <p class="muted small">
            {copied.kind === 'web'
              ? t.web_publish.copied_link({ url: copied.url })
              : t.web_publish.copied_paywall_link({ url: copied.url })}
          </p>
        {/if}
      {/if}
    </div>
  {/if}

  <!-- e2e convention 2: every page surfaces its failures on `error-message`.
       `error` was assigned in both catch blocks and rendered NOWHERE, so a
       failed hydrate or a refused toggle was invisible to the user AND to the
       harness — a test that timed out waiting for the toggle reported
       `error=None`, which reads as "nothing went wrong" rather than "nobody
       asked". -->
  {#if error}
    <p class="error" data-testid={IDS.ERROR_MESSAGE}>{error}</p>
  {/if}
</section>

<style>
  .section { margin-bottom: 2rem; }
  .section h2 { margin-bottom: 0.75rem; font-size: 1.125rem; }
  .muted { color: var(--text-muted); }
  .small { font-size: 0.8rem; margin-top: 0.25rem; }
  .toggle-row { margin-bottom: 1rem; }
  .toggle {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0.375rem 1rem;
    color: var(--text);
    cursor: pointer;
  }
  .toggle.on {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--bg);
  }
  .url-row {
    display: flex;
    gap: 0.5rem;
    align-items: baseline;
    margin-bottom: 1rem;
  }
  .caption { color: var(--text-muted); font-size: 0.875rem; }
  .url { font-family: var(--font-mono, monospace); font-size: 0.875rem; }
  .error { color: var(--danger, #c33); font-size: 0.875rem; }
</style>
