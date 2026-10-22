<script lang="ts">
  // The admin's hosted plugins on the Bridges page (`bridges.md` § Hosted
  // plugins on this page): one `bridge-plugin-item` per installed plugin — its
  // name, publisher and runner status, each composed once in shared Rust
  // (`fauna_client_bridges::installed_plugin_text`) — Uninstall behind an inline
  // confirm, then the install entry, the document URL an admin gives
  // (`third-party.md` § The runner contract → *The install-approval leg*).
  // Reference painter: `apps/fauna-tui/src/bridges.rs::plugin_elements`. The
  // parent mounts this only while the account is the nest's admin.
  import { onMount } from 'svelte';
  import { connectionStatus, identity } from '#lib/store';
  import { offlineAffordance } from '#lib/wasm';
  import { makeOfflineGate } from '#lib/offline-gate';
  import { resolveLocalized } from '#lib/i18n/localized';
  import {
    pluginsList,
    pluginsInstall,
    pluginsUninstall,
    type InstalledPluginText,
  } from '#lib/rpc';
  import { refusalText } from '#lib/guardian-refusal';
  import { t } from '#lib/i18n/strings';
  import { IDS } from '#lib/generated/uiIds';

  let { error = $bindable('') } = $props();

  // Install and Uninstall are `OnlineOnly`: the offline gate greys them.
  const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);

  // `null` until the read returned: nothing is painted, then rows or the empty line.
  let plugins = $state<InstalledPluginText[] | null>(null);
  let installUrl = $state('');
  let installOpened = $state(false);
  // The install row whose inline uninstall confirm is open.
  let armed = $state<string | null>(null);

  async function refresh() {
    const secret = $identity?.secretHex;
    if (!secret) return;
    try {
      plugins = await pluginsList(secret);
    } catch (e) {
      error = refusalText(e) || t.common.error;
    }
  }

  onMount(refresh);

  async function install() {
    const secret = $identity?.secretHex;
    const url = installUrl.trim();
    if (!secret || !url) return;
    installOpened = false;
    error = '';
    try {
      await pluginsInstall(secret, url);
      installUrl = '';
      installOpened = true;
    } catch (e) {
      error = refusalText(e) || t.common.error;
    }
  }

  async function uninstall(row: InstalledPluginText) {
    const secret = $identity?.secretHex;
    if (!secret) return;
    if (armed !== row.principal_id_hex) {
      armed = row.principal_id_hex;
      return;
    }
    armed = null;
    try {
      await pluginsUninstall(secret, row.principal_id_hex);
      await refresh();
    } catch (e) {
      error = refusalText(e) || t.common.error;
    }
  }
</script>

<section class="plugins">
  <h3>{t.bridges.plugins_heading}</h3>
  {#if plugins !== null}
    {#if plugins.length === 0}
      <p class="muted">{t.bridges.plugins_empty}</p>
    {/if}
    {#each plugins as row (row.principal_id_hex)}
      <div class="card" data-testid={IDS.BRIDGE_PLUGIN_ITEM}>
        <h4 data-testid={IDS.BRIDGE_PLUGIN_NAME} data-client_id={row.client_id}>{row.name}</h4>
        <p data-testid={IDS.BRIDGE_PLUGIN_PUBLISHER}>{resolveLocalized(row.publisher)}</p>
        <p class="muted" data-testid={IDS.BRIDGE_PLUGIN_STATUS} data-state={row.state} style="white-space: pre-line">{row.status.map(resolveLocalized).join('\n')}</p>
        {#if armed === row.principal_id_hex}
          <p>{t.bridges.plugin_uninstall_prompt({ name: row.name })}</p>
        {/if}
        <div class="actions">
          <button
            class="btn danger"
            data-testid={IDS.BRIDGE_PLUGIN_UNINSTALL}
            use:offlineGate={{ kind: 'fauna.plugins.uninstall' }}
            onclick={() => uninstall(row)}
          >{armed === row.principal_id_hex ? t.bridges.plugin_uninstall_confirm : t.bridges.plugin_uninstall}</button>
          {#if armed === row.principal_id_hex}
            <button class="btn" onclick={() => (armed = null)}>{t.bridges.plugin_uninstall_cancel}</button>
          {/if}
        </div>
      </div>
    {/each}
  {/if}
  <div class="actions">
    <input
      class="input"
      data-testid={IDS.BRIDGE_PLUGIN_INSTALL_URL}
      placeholder={t.bridges.plugin_install_label}
      aria-label={t.bridges.plugin_install_label}
      bind:value={installUrl}
      onkeydown={(e) => { if (e.key === 'Enter') install(); }}
    />
    <button
      class="btn btn-primary"
      data-testid={IDS.BRIDGE_PLUGIN_INSTALL_SUBMIT}
      use:offlineGate={{ kind: 'fauna.plugins.install', disabled: !installUrl.trim() }}
      onclick={install}
    >{t.bridges.plugin_install_submit}</button>
  </div>
  {#if installOpened}
    <p class="muted">{t.bridges.plugin_install_opened}</p>
  {/if}
</section>
