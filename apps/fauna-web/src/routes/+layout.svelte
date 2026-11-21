<script lang="ts">
  // The root layout: the identity store's unlock gate around the app shell
  // (`#lib/components/RootShell.svelte`). Until the store serves reads the gate
  // renders the `unlock` page in the shell's place (`UnlockGate.svelte`).
  import '../app.css';
  import UnlockGate from '#lib/components/UnlockGate.svelte';
  import RootShell from '#lib/components/RootShell.svelte';
  import { beforeNavigate } from '$app/navigation';
  import { oweNavigation, trackNavigation } from '#lib/navigation-in-flight';

  let { children } = $props();

  if (typeof window !== 'undefined' && __FAUNA_E2E_AUTOMATION__) {
    // The e2e barrier's navigation leg (`#lib/navigation-in-flight`): it must
    // be registered here, while the layout initialises. A navigation that
    // unloads the document never settles in this tab, so it is not counted.
    beforeNavigate((nav) => {
      if (!nav.willUnload) trackNavigation(nav.complete);
    });
    // A link click the router claimed owes a navigation it begins only a task
    // or more later (`#lib/navigation-in-flight`, *Why a claimed click is owed
    // a navigation*). On `window`, bubbling, this runs after the router's own
    // document listener: a click it took for a client navigation arrives
    // default-prevented, on a same-origin link the browser would otherwise
    // have followed in this tab.
    window.addEventListener('click', (ev) => {
      if (!ev.defaultPrevented || ev.button !== 0) return;
      if (ev.metaKey || ev.ctrlKey || ev.shiftKey || ev.altKey) return;
      const a = (ev.target as Element | null)?.closest?.('a[href]');
      if (!(a instanceof HTMLAnchorElement)) return;
      if (a.hasAttribute('download') || (a.target && a.target !== '_self')) return;
      if (a.getAttribute('data-sveltekit-reload') !== null) return;
      const url = new URL(a.href, location.href);
      if (url.origin !== location.origin) return;
      // Same document with only the hash moved: no navigation to wait for.
      if (url.pathname === location.pathname && url.search === location.search && url.hash) return;
      oweNavigation();
    });
    // Test builds only (testing.md § Test-agent build exclusion): install the
    // whole `window.__fauna_*` automation surface — here, ahead of the gate, so
    // a journey can drive the unlock page too. A production `vite build`
    // constant-folds this flag to false and emits neither this branch nor the
    // dynamic chunk, so the hooks are absent from the shipped bundle — not
    // merely inert.
    void import('#lib/e2e-automation').then((m) => m.installAutomationSurface());
  }
</script>

<UnlockGate>
  <RootShell {children} />
</UnlockGate>
