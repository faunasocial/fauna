<script lang="ts">
  // Shared sidebar-shell chrome for web's three sidebar-swap layouts
  // (routes/+layout.svelte, routes/admin/+layout.svelte,
  // routes/settings/+layout.svelte) — the flex .shell/.sidebar/.content
  // frame every one of them used to hand-copy, each own style block naming
  // the other two and asking a human to keep them in sync by hand (named
  // three times over, never actually built — priority #2).
  //
  // Deliberately narrow: only the byte-identical-everywhere frame lives
  // here — .shell, the sidebar's base box model, .content, and their
  // mobile counterparts. Everything that differs per shell — .tab and its
  // hover/active states (real gap/padding/font-size/transition deltas
  // between the three), the connection-status indicator (root + admin
  // only; settings renders none), nav-back/title/logo chrome — stays in
  // each caller's own style block, scoped to markup the caller defines
  // in its own sidebar/main snippet: Svelte scopes a snippet's elements
  // to the component that AUTHORED the snippet, not the one that renders
  // it, so this frame can render the callers' markup without owning its
  // styling. A global (unscoped) stylesheet was rejected for exactly the
  // risk that scoping avoids — profile/+page.svelte already has its own,
  // differently-styled .tab class, which a global rule would corrupt.
  //
  // `sidebarScrollable` is the one real per-shell delta in the shared base
  // itself: the settings rail is long enough to need its own vertical
  // scroll (`.sidebar { overflow-y: auto }`); root's and admin's shorter
  // rails don't carry it.
  //
  // `mobileSidebarLayout` picks the 768px sidebar's flex behavior: root's
  // fixed, short row of icon tabs spaces out (`justify-content:
  // space-around`, the default); admin's and settings' longer text-label
  // rails scroll horizontally instead (`overflow-x: auto; align-items:
  // center`).
  import type { Snippet } from 'svelte';

  let {
    sidebar,
    main,
    sidebarScrollable = false,
    mobileSidebarLayout = 'spaced',
  }: {
    sidebar: Snippet;
    main: Snippet;
    sidebarScrollable?: boolean;
    mobileSidebarLayout?: 'spaced' | 'scrolling';
  } = $props();
</script>

<div class="shell">
  <nav
    class="sidebar"
    class:scrollable={sidebarScrollable}
    class:scrolling-mobile={mobileSidebarLayout === 'scrolling'}
  >
    {@render sidebar()}
  </nav>
  <main class="content">
    {@render main()}
  </main>
</div>

<style>
  .shell {
    display: flex;
    height: 100vh;
  }
  .sidebar {
    width: 200px;
    background: var(--bg-surface);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    padding: 1rem 0;
  }
  .sidebar.scrollable {
    overflow-y: auto;
  }
  .content {
    flex: 1;
    overflow-y: auto;
    padding: 1.5rem;
  }

  @media (max-width: 768px) {
    .shell {
      flex-direction: column-reverse;
    }
    .sidebar {
      width: 100%;
      flex-direction: row;
      border-right: none;
      border-top: 1px solid var(--border);
      padding: 0;
      justify-content: space-around;
    }
    .sidebar.scrolling-mobile {
      justify-content: flex-start;
      overflow-x: auto;
      align-items: center;
    }
    .content {
      padding: 1rem;
    }
  }
</style>
