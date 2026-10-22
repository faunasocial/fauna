<script lang="ts">
  // The root layout: the identity store's unlock gate around the app shell
  // (`#lib/components/RootShell.svelte`). Until the store serves reads the gate
  // renders the `unlock` page in the shell's place (`UnlockGate.svelte`).
  import '../app.css';
  import UnlockGate from '#lib/components/UnlockGate.svelte';
  import RootShell from '#lib/components/RootShell.svelte';

  let { children } = $props();

  if (typeof window !== 'undefined' && __FAUNA_E2E_AUTOMATION__) {
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
