<script lang="ts">
  // The ui.yaml GLOBAL `connection-status` element ("MUST be present on every
  // authenticated page in all 7 apps") — ONE component for web's three
  // sidebar shells: the root sidebar (RootShell), and the two sidebar-SWAP
  // rails (admin and settings), where the root layout yields the viewport and
  // its own indicator goes with it. Each shell used to hand-copy this block;
  // settings never got it, so a web user on any Settings page — muted words,
  // devices, every OnlineOnly control there — could not see the link drop.
  //
  // The state → label decision is the shared
  // `fauna_core::format::connection_state_label` over wasm (priority #2), never
  // a web-local ternary: a transient swap reads "Connecting…", a connection that
  // has failed to establish repeatedly reads "Cannot connect". Until the wasm
  // module is up the label is "Disconnected" — what `connectionStatus` holds
  // until a client is built anyway. Every shell reads the same store, so the
  // indicators and the offline gate cannot disagree.
  // transport.md § Connection-status indicator.
  import { onMount } from 'svelte';
  import { connectionStatus } from '#lib/store';
  import { connectionStateLabel, ensureWasm } from '#lib/wasm';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let wasmReady = $state(false);
  onMount(async () => {
    await ensureWasm();
    wasmReady = true;
  });

  let label = $derived(
    wasmReady ? resolveLocalized(connectionStateLabel($connectionStatus)) : t.common.disconnected,
  );
</script>

<div
  class="connection-status"
  class:connected={$connectionStatus === 'connected'}
  data-testid={IDS.CONNECTION_STATUS}
>
  {label}
</div>

<style>
  .connection-status {
    font-size: 0.75rem;
    padding: 0 1rem 0.75rem;
    color: var(--text-muted);
  }
  .connection-status.connected {
    color: var(--text);
  }
</style>
