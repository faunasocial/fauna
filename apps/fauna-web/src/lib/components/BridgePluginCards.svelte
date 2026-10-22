<script lang="ts">
  // Every installed plugin's settings card on the Bridges page, one more
  // `bridge-card` after the bridges (`third-party.md` § The principal model →
  // *Hosted principals*; `bridges.md` § Hosted plugins on this page): the
  // shared `plugin_card_text`'s name, badge and publisher as text, its verb as
  // the card's `bridge-action-button` — Connect while unbound, Disconnect
  // while bound, carrying the plugin's `client_id` and a `bound` value so a
  // reader can tell the cards apart — and, once bound, its settings through the
  // one settings renderer (`BridgeSettingRows`). Connect and Disconnect are the
  // connected-apps machine's `bindPlugin` / `revokePrincipal`: the bind card
  // waits in Connected apps → Requests, answered like every card. Reference
  // painter: `apps/fauna-tui/src/bridges.rs::plugin_card_elements`. Shown to
  // every account.
  import { onDestroy, onMount } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import { connectionStatus, identity } from '#lib/store';
  import { offlineAffordance, ensureWasm } from '#lib/wasm';
  import { makeOfflineGate } from '#lib/offline-gate';
  import { resolveLocalized } from '#lib/i18n/localized';
  import { pluginsAvailable, pluginsSetSetting, sharedRpcPort, type PluginCardText } from '#lib/rpc';
  import { createConnectedAppsMachine, type ConnectedAppsMachine } from '#lib/wasm-connected-apps';
  import BridgeSettingRows from '#lib/components/BridgeSettingRows.svelte';
  import { refusalText } from '#lib/guardian-refusal';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let { error = $bindable('') } = $props();

  // Connect and Disconnect are `OnlineOnly`: the offline gate greys them.
  const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);

  // `null` until the read returned: nothing is painted, then the cards.
  let cards = $state<PluginCardText[] | null>(null);
  let bindOpened = $state(false);
  let machine: ConnectedAppsMachine | null = null;
  let destroyed = false;

  async function refresh() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      const next = await pluginsAvailable(secret);
      if (!destroyed) cards = next;
    } catch (e) {
      error = refusalText(e) || t.common.error;
    }
  }

  onMount(refresh);
  // A bind approved on Connected apps pushes nothing here, so every visit —
  // including one to this very route — re-reads.
  afterNavigate(refresh);

  onDestroy(() => {
    destroyed = true;
    machine?.free();
    machine = null;
  });

  /** The machine behind the card's verbs, built on first use over the SPA
   *  socket; its page error (what a refused verb set) is what the card shows. */
  async function verbMachine(secret: string): Promise<ConnectedAppsMachine> {
    if (!machine) {
      await ensureWasm();
      machine = await createConnectedAppsMachine({ onChanged: () => {} }, await sharedRpcPort(secret), secret);
    }
    return machine;
  }

  function machineError(m: ConnectedAppsMachine): string {
    const raw = m.snapshotJson();
    const snap = raw ? (JSON.parse(raw) as { error?: Parameters<typeof resolveLocalized>[0] | null }) : null;
    return snap?.error ? resolveLocalized(snap.error) : '';
  }

  async function press(card: PluginCardText) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    bindOpened = false;
    error = '';
    try {
      const m = await verbMachine(secret);
      if (card.bound && card.binding_principal_id_hex) {
        await m.revokePrincipal(card.binding_principal_id_hex);
      } else {
        await m.bindPlugin(card.principal_id_hex);
      }
      const failed = machineError(m);
      if (failed) {
        error = failed;
        return;
      }
      if (!card.bound) bindOpened = true;
      await refresh();
    } catch (e) {
      error = refusalText(e) || t.common.error;
    }
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  async function setSetting(card: PluginCardText, key: string, value: any) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      await pluginsSetSetting(secret, card.principal_id_hex, key, value);
      await refresh();
    } catch (e) {
      error = refusalText(e) || t.common.error;
    }
  }
</script>

{#if cards !== null}
  {#each cards as card (card.principal_id_hex)}
    <div class="card" data-testid={IDS.BRIDGE_CARD}>
      <h3>{card.name}</h3>
      <p class="muted" style="white-space: pre-line">{resolveLocalized(card.badge)}{'\n'}{resolveLocalized(card.publisher)}</p>
      {#if card.bound}
        <BridgeSettingRows settings={card.settings} onSettingChange={(key, value) => setSetting(card, key, value)} />
      {/if}
      <!-- One button per state, each declaring the wire kind it issues. -->
      {#if card.bound}
        <button
          data-testid={IDS.BRIDGE_ACTION_BUTTON}
          data-client_id={card.client_id}
          data-bound="true"
          class="btn danger"
          use:offlineGate={{ kind: 'fauna.principals.revoke' }}
          onclick={() => press(card)}
        >{t.bridges.plugin_disconnect}</button>
      {:else}
        <button
          data-testid={IDS.BRIDGE_ACTION_BUTTON}
          data-client_id={card.client_id}
          data-bound="false"
          class="btn primary"
          use:offlineGate={{ kind: 'fauna.plugins.bind' }}
          onclick={() => press(card)}
        >{t.bridges.plugin_connect}</button>
      {/if}
    </div>
  {/each}
  {#if bindOpened}
    <p class="muted">{t.bridges.plugin_bind_opened}</p>
  {/if}
{/if}

<style>
  .card {
    background: var(--surface, #1a1a2e);
    border-radius: 8px;
    padding: 1rem;
    margin-bottom: 1rem;
  }
  .btn {
    padding: 0.4rem 0.8rem;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    background: var(--accent, #444);
    color: var(--text, #eee);
  }
  .btn.primary { background: #4a6cf7; }
  .btn.danger { background: #c0392b; }
  .muted { color: var(--muted, #888); }
</style>
