<script lang="ts">
  // One bridge's card — link status/action button, per-bridge settings rows,
  // follows list (linked) or the metadata-driven link form (unlinked). The
  // `bridge-card`/`bridge-link-form` shared components (ui.yaml, `used_in:
  // [bridges, bluesky]`) — extracted out of routes/bridges/+page.svelte so the
  // AT Protocol page's Linked-account panel (level = linked) can embed the SAME
  // widget for a single provider, mirroring linux's
  // `views::bridges::detail::build_bridge_detail_content` /
  // apple's `BridgeCardContent` extraction (docs/goal/ui/atproto.md § Layout &
  // flow, § Element IDs — zero new IDs, second host page).
  //
  // Self-contained: mode filtering (platform + NIP-07 availability) and the
  // NIP-07 pubkey fetch are link-mechanism concerns, not page concerns, so
  // they live here rather than being duplicated by every host page. The
  // actual RPC calls + error surfacing stay with the host page (its `error`
  // binding shape differs — MessageBanner vs. a bindable prop), passed in as
  // callback props.
  import { hasNip07, nip07LinkFields } from '$lib/nostr';
  import { identity } from '$lib/store';
  import { bridgeLinkBlock, bridgeModeApplies, parseCountI64, shortId } from '$lib/wasm';
  import { onMount } from 'svelte';
  import { t } from '$lib/i18n/strings';
  import type { BridgeInfo, BridgeFollow, BridgeLinkMode } from '$lib/bridges';
  import type { FeedTriple, SourceAskRows } from '$lib/ward-asks';
  import { IDS } from '$lib/generated/uiIds';

  let {
    bridge,
    follows = [],
    sourceAsks,
    onRequestSource,
    onLink,
    onUnlink,
    onSettingChange,
    onAddFollow,
    onRemoveFollow,
  }: {
    bridge: BridgeInfo;
    follows?: BridgeFollow[];
    /** The supervised ward's feed-source ask rows for THIS card
     *  (`$lib/ward-asks` `sourceAskRows`) — `bridge-source-request-state` per
     *  durable ask, `bridge-source-request-button` per refused triple with no
     *  durable row yet (family-safety.md § Feed-source approvals). Absent on a
     *  host page that has no ask surface (the Bluesky Linked panel). */
    sourceAsks?: SourceAskRows;
    onRequestSource?: (triple: FeedTriple) => void | Promise<void>;
    onLink: (mode: string, fields: Record<string, string>) => void | Promise<void>;
    onUnlink: () => void | Promise<void>;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    onSettingChange: (key: string, value: any) => void | Promise<void>;
    onAddFollow: (id: string, petname?: string) => void | Promise<void>;
    onRemoveFollow: (id: string) => void | Promise<void>;
  } = $props();

  let hasNip07Extension = $state(false);
  onMount(() => {
    hasNip07Extension = hasNip07();
  });

  // Link form state: this card's selected mode + typed field values.
  let linkFieldValues = $state<Record<string, string>>({});
  let selectedLinkMode = $state('');

  // Follow form state.
  let followId = $state('');
  let followPetname = $state('');

  let modes = $derived.by((): BridgeLinkMode[] => {
    if (!bridge.link_modes) return [];
    return bridge.link_modes.filter((m) => {
      // The platform string-match is the SHARED rule (lifted 2026-08-15 from
      // the seven per-app copies — `fauna_client_bridges::mode_applies`); only
      // the nip07 extension probe below stays local, a runtime browser
      // capability no shared crate can observe.
      if (!bridgeModeApplies(m.platform ?? undefined, 'web')) return false;
      if (m.client_action === 'nip07' && !hasNip07Extension) return false;
      return true;
    });
  });

  // Why linking is unavailable, from the shared rule (`null` = it IS available).
  // `modes` is already platform- AND extension-filtered above; the nip07 half is
  // a runtime browser capability no shared crate could observe, which is why the
  // count crosses rather than the list.
  let linkBlock = $derived(bridgeLinkBlock(bridge.linked, bridge.error, modes.length));

  function getSelectedMode(): BridgeLinkMode | undefined {
    if (modes.length === 0) return undefined;
    return modes.find((m) => m.mode === selectedLinkMode) ?? modes[0];
  }

  async function handleLink(): Promise<void> {
    const mode = getSelectedMode();
    if (!mode) return;
    let fields: Record<string, string> = { ...linkFieldValues };
    // NIP-07 client action: the extension reports its pubkey and signs the
    // nest's challenge — the proof of possession the link is refused without.
    if (mode.client_action === 'nip07') {
      const secret = $identity?.secretHex;
      if (!secret) return;
      fields = { ...fields, ...(await nip07LinkFields(secret, bridge.id)) };
    }
    await onLink(mode.mode, fields);
  }

  async function handleAddFollow(): Promise<void> {
    if (!followId) return;
    await onAddFollow(followId, followPetname || undefined);
    followId = '';
    followPetname = '';
  }
</script>

<div class="card" data-testid={IDS.BRIDGE_CARD}>
  <h3>{bridge.name}</h3>
  {#if !bridge.available}
    <p class="muted">{t.bridges.not_available({ name: bridge.name })}</p>
  {:else if bridge.linked}
    <!-- Identity display -->
    {#if bridge.identity}
      <p><strong>{bridge.identity.label}:</strong> <span class="mono">{bridge.identity.display}</span></p>
    {/if}

    <!-- Settings -->
    {#each bridge.settings as setting}
      {#if setting.type === 'bool'}
        <label class="toggle">
          <input type="checkbox" checked={setting.value} onchange={() => onSettingChange(setting.key, !setting.value)} />
          {setting.label}
        </label>
      {:else if setting.type === 'text'}
        <label>{setting.label}:
          <input type="text" value={setting.value} onblur={(e) => onSettingChange(setting.key, e.currentTarget.value)} class="input" />
        </label>
      {:else if setting.type === 'select' && setting.options}
        <label>{setting.label}:
          <select onchange={(e) => onSettingChange(setting.key, Number(e.currentTarget.value))}>
            {#each setting.options as opt}
              <option value={opt.value} selected={opt.value === setting.value}>{opt.label}</option>
            {/each}
          </select>
        </label>
      {:else if setting.type === 'number'}
        <label>{setting.label}:
          <input
            type="text"
            value={setting.value}
            onblur={(e) => {
              const parsed = parseCountI64(e.currentTarget.value);
              if (parsed !== undefined) onSettingChange(setting.key, parsed);
            }}
            class="input"
          />
        </label>
      {:else}
        <!-- Unrecognized setting_type: additive-evolution fallback (a newer
             nest may serve a type this build predates) — a read-only label,
             never a silently dropped row. Mirrors tui's `setting_elements`
             fallback arm. -->
        <p class="muted">{setting.label}: {setting.value}</p>
      {/if}
    {/each}

    <!-- Unlink -->
    <button data-testid={IDS.BRIDGE_ACTION_BUTTON} class="btn danger" onclick={() => onUnlink()}>{t.bridges.unlink({ name: bridge.name })}</button>

    <!-- Follows -->
    {#if bridge.supports_follows}
      <h4>{t.bridges.follows}</h4>
      <div class="follow-form">
        <input type="text" bind:value={followId} placeholder={t.bridges.id_to_follow} class="input" />
        <input type="text" bind:value={followPetname} placeholder={t.bridges.petname_optional} class="input small" />
        <button data-testid={IDS.BRIDGE_ADD_FOLLOW_BUTTON} onclick={handleAddFollow} class="btn">{t.bridges.add_follow}</button>
      </div>
      <div data-testid={IDS.BRIDGE_FOLLOWS_LIST}>
        {#if follows.length > 0}
          <ul class="follows-list">
            {#each follows as follow}
              <li data-testid={IDS.BRIDGE_FOLLOW_ITEM}>
                <span class="mono">{shortId(follow.id)}</span>
                {#if follow.petname}
                  <span class="petname">({follow.petname})</span>
                {/if}
                <button data-testid={IDS.BRIDGE_FOLLOW_REMOVE} onclick={() => onRemoveFollow(follow.id)} class="btn small danger">{t.bridges.remove_follow}</button>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="muted">{t.bridges.no_follows}</p>
        {/if}
      </div>
      <!-- Bridge feed subscription lives on the Feed page (bridge-form-*),
           not the bridge card (bridges.md § Layout & flow, Feed-only decision
           ratified 2026-06-28). The former bridge-feed-* shim elements were
           retired with the ui.yaml registry removal. -->
    {/if}
  {:else}
    <!-- Unlinked: show link modes. The Link button always renders when
         unlinked, gated only on `linked` — never on whether the nest has
         declared any modes yet (matches linux's build_bridge_detail_content
         / tui's bridge_card; this is what makes a dedicated-page embed of a
         provider the nest hasn't registered a feature for yet — e.g. the
         Bluesky Linked panel pre-fetch — render an honest, clickable
         surface rather than silently vanishing). -->
    {#if modes.length > 1}
      <div class="link-options">
        {#each modes as mode}
          <label>
            <input
              type="radio"
              name="link-mode-{bridge.id}"
              value={mode.mode}
              checked={(selectedLinkMode || modes[0].mode) === mode.mode}
              onchange={() => { selectedLinkMode = mode.mode; }}
            />
            {mode.label}
          </label>
        {/each}
      </div>
    {/if}

    <!-- Fields for selected mode -->
    {@const currentMode = getSelectedMode()}
    {#if currentMode}
      {#each currentMode.fields as field}
        <input
          data-testid="bridge-link-field-{field.key}"
          type={field.type === 'password' ? 'password' : 'text'}
          placeholder={field.placeholder ?? field.label}
          value={linkFieldValues[field.key] ?? ''}
          oninput={(e) => { linkFieldValues[field.key] = e.currentTarget.value; }}
          class="input"
        />
      {/each}
    {/if}
    {#if linkBlock}
      <p class="muted" data-testid={IDS.BRIDGE_LINK_BLOCKED_REASON}>
        {linkBlock.kind === 'provider_error' ? linkBlock.message : t.bridges.no_link_method}
      </p>
    {/if}

    <button
      data-testid={IDS.BRIDGE_ACTION_BUTTON}
      class="btn primary"
      disabled={linkBlock !== null}
      onclick={handleLink}>{t.bridges.link({ name: bridge.name })}</button>
  {/if}

  <!-- The ward's feed-source asks on this card (family-safety.md § Feed-source
       approvals). Paints nothing in the common case — both inputs are
       supervised-only by construction. An approved ask is the "try again"
       PROMPT, a label and never a button: the grant is single-use and the ward
       redeems it by retrying the original Link / Add Follow above; nothing
       here ever retries on its own. -->
  {#if sourceAsks}
    {#each sourceAsks.states as state}
      <p class="muted" data-testid={IDS.BRIDGE_SOURCE_REQUEST_STATE}>
        {state === 'approved' ? t.bridges.source_request_approved : t.bridges.source_request_pending}
      </p>
    {/each}
    {#each sourceAsks.asks as triple}
      <button
        data-testid={IDS.BRIDGE_SOURCE_REQUEST_BUTTON}
        class="btn"
        onclick={() => onRequestSource?.(triple)}
      >{t.bridges.source_request_button}</button>
    {/each}
  {/if}
</div>

<style>
  .card {
    background: var(--surface, #1a1a2e);
    border-radius: 8px;
    padding: 1rem;
    margin-bottom: 1rem;
  }
  .mono {
    font-family: monospace;
    font-size: 0.85em;
    word-break: break-all;
  }
  .link-options {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin: 0.75rem 0;
  }
  .input {
    width: 100%;
    padding: 0.5rem;
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    background: var(--bg, #0f0f23);
    color: var(--text, #eee);
    margin: 0.5rem 0;
  }
  .input.small {
    width: 200px;
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
  .btn.small { padding: 0.2rem 0.5rem; font-size: 0.85em; }
  .toggle {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0.4rem 0;
  }
  .follow-form {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    margin-bottom: 0.75rem;
  }
  .follows-list {
    list-style: none;
    padding: 0;
  }
  .follows-list li {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.3rem 0;
    border-bottom: 1px solid var(--border, #222);
  }
  .petname { color: var(--muted, #888); }
  .muted { color: var(--muted, #888); }
</style>
