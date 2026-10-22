<script lang="ts">
  // The metadata-driven settings rows of a bridge card (`bridges.md` § Bridge
  // settings) — extracted out of `BridgeCard.svelte` so a hosted plugin's
  // settings card (`BridgePluginCards.svelte`) renders the SAME rows: one
  // renderer, as tui's `setting_elements` is. The caller owns the write
  // (`fauna.bridges.set_settings` for a bridge, `fauna.plugins.set_settings`
  // for a plugin).
  import { bridgeSettingControl, parseCountI64 } from '#lib/wasm';
  import type { BridgeSetting } from '#lib/bridges';
  import { IDS } from '#lib/generated/uiIds';

  let {
    settings,
    onSettingChange,
  }: {
    settings: BridgeSetting[];
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    onSettingChange: (key: string, value: any) => void | Promise<void>;
  } = $props();

  // A row is `bridge-setting[<key>]` — keyed by the setting, whatever control
  // it paints (ui.yaml `elements.bridge-setting`; the `settings-nav-row[<key>]`
  // idiom). A typed value commits when it differs from the saved one, on
  // blur, Enter or a click on the field: an agent's click on an editable is
  // its commit (tui's convention), and a click that changes nothing sends
  // nothing.
  const rowId = (key: string) => `${IDS.BRIDGE_SETTING}[${key}]`;
  function commitText(setting: BridgeSetting, typed: string): void {
    if (typed !== String(setting.value ?? '')) onSettingChange(setting.key, typed);
  }
  function commitNumber(setting: BridgeSetting, typed: string): void {
    const parsed = parseCountI64(typed);
    if (parsed !== undefined && String(parsed) !== String(setting.value ?? '')) {
      onSettingChange(setting.key, parsed);
    }
  }
</script>

{#each settings as setting}
  {@const control = bridgeSettingControl(setting.type)}
  <!-- The control comes from the shared classifier (`setting_control`) — no
       spelling is matched here. -->
  {#if control === 'toggle'}
    <label class="toggle">
      <input type="checkbox" data-testid={rowId(setting.key)} data-checked={setting.value ? 'true' : 'false'} checked={setting.value} onchange={() => onSettingChange(setting.key, !setting.value)} />
      {setting.label}
    </label>
  {:else if control === 'text'}
    <label>{setting.label}:
      <input
        type="text"
        data-testid={rowId(setting.key)}
        value={setting.value}
        onblur={(e) => commitText(setting, e.currentTarget.value)}
        onclick={(e) => commitText(setting, e.currentTarget.value)}
        onkeydown={(e) => { if (e.key === 'Enter') commitText(setting, e.currentTarget.value); }}
        class="input"
      />
    </label>
  {:else if control === 'select' && setting.options}
    <label>{setting.label}:
      <select data-testid={rowId(setting.key)} onchange={(e) => onSettingChange(setting.key, Number(e.currentTarget.value))}>
        {#each setting.options as opt}
          <option value={opt.value} selected={opt.value === setting.value}>{opt.label}</option>
        {/each}
      </select>
    </label>
  {:else if control === 'number'}
    <label>{setting.label}:
      <input
        type="text"
        data-testid={rowId(setting.key)}
        value={setting.value}
        onblur={(e) => commitNumber(setting, e.currentTarget.value)}
        onclick={(e) => commitNumber(setting, e.currentTarget.value)}
        onkeydown={(e) => { if (e.key === 'Enter') commitNumber(setting, e.currentTarget.value); }}
        class="input"
      />
    </label>
  {:else}
    <!-- Unrecognized setting_type: additive-evolution fallback (a newer
         nest may serve a type this build predates) — a read-only label,
         never a silently dropped row. Mirrors tui's `setting_elements`
         fallback arm. -->
    <p class="muted" data-testid={rowId(setting.key)}>{setting.label}: {setting.value}</p>
  {/if}
{/each}

<style>
  .input {
    width: 100%;
    padding: 0.5rem;
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    background: var(--bg, #0f0f23);
    color: var(--text, #eee);
    margin: 0.5rem 0;
  }
  .muted { color: var(--muted, #888); }
  .toggle {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0.4rem 0;
  }
</style>
