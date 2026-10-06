<script lang="ts">
  import { connectionStatus, identity } from '$lib/store';
  import { connectionStateLabel, ensureWasm } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';
  import AppShellFrame from '$lib/components/AppShellFrame.svelte';

  // The admin shell is a vertical **sidebar-swap** (admin.md § Navigation
  // model, ratified 2026-06-01, vertical-rail form 2026-06-03): while in the
  // admin shell this layout renders its OWN shell whose vertical rail takes
  // over the app's sidebar slot — the root layout (routes/+layout.svelte)
  // yields the full viewport for any `/app/admin` route (same bare-canvas
  // bypass it uses for onboarding), so the normal nav (conversations / feed /
  // …) is replaced in place rather than sitting beside a horizontal admin
  // nav-bar. `admin-nav-back` (atop the rail) swaps it back. The shared
  // `.shell`/`.sidebar`/`.content` frame lives in `AppShellFrame` (this
  // shell's own `.tab`/`.connection-status`/etc. stay here — see that
  // component's doc comment). Reference: linux `views/admin.rs` (the
  // StackSidebar rail) + windows (NavigationView PaneDisplayMode=Left).

  let { children } = $props();
  let booting = $state(true);
  let wasmReady = $state(false);

  // Same derivation as the root layout's (keep the two in sync, as the shell
  // shape already is): the state → label decision is the shared
  // `fauna_core::format::connection_state_label` over wasm, never a web-local
  // ternary, so this rail and the root sidebar cannot disagree about what
  // "connected" means — and neither can the offline gate, which reads the same
  // store. transport.md § Connection-status indicator.
  let connectionLabel = $derived(
    wasmReady ? resolveLocalized(connectionStateLabel($connectionStatus)) : t.common.disconnected,
  );

  // The `am-i-admin` gate is on the nav ENTRY, never on this shell's content
  // (admin.md § Shell + § Navigation model point 1): the root layout's
  // `userIsAdmin` effect decides whether `admin-tab` renders, and that is the
  // whole gate — on every one of the 7 apps. Web used to additionally re-run
  // `checkIsAdmin` here and `goto('/app/settings')` on a negative, which
  // unmounted every admin child page before it could render. That was a
  // CONTENT-level gate no other app has (linux `app.rs` name-nav to "admin" is
  // ungated and only `show_admin_sidebar_row` reads `is_admin`; tui gates only
  // the visible sidebar set; windows gates the entry in `MainPage`), and no
  // line of admin.md ever described it — priority #1's lone web deviation,
  // removed here. It was never the security boundary either: every admin kind
  // is `require_admin` nest-side (`admin_ws_handlers.rs`), so a non-admin who
  // reaches an admin page gets a rejection to render, which is exactly what
  // `error-message` is for (e2e-conventions.md convention 2) and what
  // `test_admin_error_surfacing.py` asserts on every app.
  //
  // What stays is the AUTHENTICATION gate — no identity means no app at all,
  // admin or otherwise — plus the wasm/identity boot barrier the rail's own
  // connection label needs.
  onMount(async () => {
    await ensureWasm();
    wasmReady = true;
    identity.init();
    if (!$identity?.secretHex) { goto('/app/settings'); return; }
    booting = false;
  });

  // Labels come from the shared i18n strings (en.yaml), mirroring the exact key
  // each native app already uses for the same admin-shell nav entry
  // (apple `Core/AdminPage.swift` label switch; windows `AdminShellPage.xaml`;
  // android admin nav) so all 7 apps render the same text off the same keys
  // (priority #1/#3). No web-only keys are minted — every key already exists.
  const navLinks = [
    { href: '/app/admin', label: t.admin.dashboard.title },
    { href: '/app/admin/users', label: t.admin.users_page.title },
    { href: '/app/admin/aliases', label: t.admin.aliases },
    { href: '/app/admin/settings', label: t.admin.settings_page.title },
    { href: '/app/admin/nest', label: t.admin.nest_page.title },
    { href: '/app/admin/dns', label: t.admin.dns.title },
    { href: '/app/admin/custody-hosting', label: t.admin.custody_hosting.title },
    { href: '/app/admin/mail', label: t.admin.mail_page.title },
    { href: '/app/admin/calendar', label: t.admin.calendar_page.title },
    { href: '/app/admin/contacts', label: t.admin.contacts_page.title },
    { href: '/app/admin/files', label: t.admin.files_page.title },
    { href: '/app/admin/web', label: t.admin.web_page.title },
    { href: '/app/admin/bridges-pending', label: t.admin.bridges_pending.title },
    { href: '/app/admin/logs', label: t.admin.logs_page.title },
  ];

  function isActive(path: string, href: string): boolean {
    if (href === '/app/admin') return path === '/app/admin' || path === '/app/admin/';
    return path.startsWith(href);
  }
</script>

{#if booting}
  <div class="loading">{t.common.loading}</div>
{:else}
  {#snippet sidebarContent()}
    <a href="/app/conversations" class="nav-back" data-testid={IDS.ADMIN_NAV_BACK}>
      <span class="icon">‹</span>
      <span class="label">{t.admin.exit}</span>
    </a>
    <div class="admin-title">{t.settings.nest_admin}</div>
    <!-- `connection-status` is a ui.yaml GLOBAL element ("MUST be present on
         every authenticated page in all 7 apps"), and the admin shell is a
         sidebar-SWAP: the root layout yields the viewport here, so its own
         indicator goes with it. Without this the admin plane — where almost
         everything is OnlineOnly — was the one place a web admin could not
         see the link drop. Same store, same shared label fn as the root
         layout's, so the two cannot disagree. -->
    <div
      class="connection-status"
      class:connected={$connectionStatus === 'connected'}
      data-testid={IDS.CONNECTION_STATUS}
    >
      {connectionLabel}
    </div>
    <div class="admin-links">
      {#each navLinks as link}
        <a
          href={link.href}
          class="tab"
          class:active={isActive($page.url.pathname, link.href)}
          data-testid="admin-nav-{link.href.split('/').pop()}"
        >{link.label}</a>
      {/each}
    </div>
  {/snippet}
  {#snippet mainContent()}
    {@render children?.()}
  {/snippet}
  <AppShellFrame sidebar={sidebarContent} main={mainContent} mobileSidebarLayout="scrolling" />
{/if}

<style>
  .loading {
    padding: 2rem;
    color: var(--text-muted, #8b949e);
    font-size: 0.875rem;
  }
  .nav-back {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 1rem;
    color: var(--text-muted);
    font-size: 0.875rem;
    transition: background 0.15s, color 0.15s;
  }
  .nav-back:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .nav-back .icon { font-size: 1.125rem; line-height: 1; }
  .admin-title {
    font-size: 1.25rem;
    font-weight: 700;
    padding: 0.5rem 1rem 1rem;
    color: var(--accent);
  }
  /* Mirrors routes/+layout.svelte's indicator, like the rest of this shell. */
  .connection-status {
    font-size: 0.75rem;
    padding: 0 1rem 0.75rem;
    color: var(--text-muted);
  }
  .connection-status.connected {
    color: var(--text);
  }
  .admin-links {
    display: flex;
    flex-direction: column;
    flex: 1;
  }
  .tab {
    display: flex;
    align-items: center;
    padding: 0.625rem 1rem;
    color: var(--text-muted);
    transition: background 0.15s, color 0.15s;
  }
  .tab:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .tab.active {
    color: var(--accent);
    background: var(--bg-hover);
  }

  @media (max-width: 768px) {
    .admin-title { display: none; }
    .nav-back { padding: 0.5rem 0.75rem; white-space: nowrap; }
    .nav-back .label { display: none; }
    .admin-links { flex-direction: row; flex: 1; }
    .tab { padding: 0.5rem 0.75rem; font-size: 0.8rem; white-space: nowrap; }
  }
</style>
