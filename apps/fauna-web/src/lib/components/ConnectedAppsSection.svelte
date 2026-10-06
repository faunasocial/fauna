<script lang="ts">
  // Settings → Connected apps (`docs/goal/ui/connected-apps.md`; ui.yaml page
  // `connected-apps`). The one roster of everything acting for the user from
  // outside the apps, plus the two app halves of the polled consent starts.
  // Four regions, top to bottom: Requests (only while a request is live — never
  // a "no requests" row), Connect an app (the typed-code start), the roster
  // (Revoke with an inline confirm), Blocked apps (only while something is
  // blocked).
  //
  // A paint shell over the shared `ConnectedAppsMachine`
  // (`libs/fauna-client-connected-apps`, its own wasm chunk —
  // `$lib/wasm-connected-apps`): roster composition, scope words, the class key,
  // *lasts-until* and which verb revokes a row are the machine's, so this file
  // never picks a revoke verb — a row's `key` is opaque. Reference painter:
  // `apps/fauna-tui/src/settings/connected_apps.rs`.
  //
  // The lift: the AT Protocol page's consent cards + connected-app rows and the
  // Nostr page's bunker rows render HERE and no longer on their old pages. The
  // mail app-password rows do NOT reach web yet — this chunk's machine is built
  // without the mail machine, so they stay on Mail & Calendar.
  import { onDestroy, onMount } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import { identity } from '$lib/store';
  import { sharedRpcPort } from '$lib/rpc';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { consentSetLines } from '$lib/consent-sets';
  import {
    createConnectedAppsMachine,
    type ConnectedAppRow,
    type ConnectedAppsMachine,
    type ConnectedAppsSnapshot,
    type ConsentCardRow,
  } from '$lib/wasm-connected-apps';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // The parent settings page owns the single error surface (`error-message`).
  let { error = $bindable('') } = $props();

  let machine: ConnectedAppsMachine | null = null;
  // `null` until the first fold: a visit paints neither rows nor the empty state
  // until its own read has returned (`connected-apps.md` § Errors & edge cases).
  let snap = $state<ConnectedAppsSnapshot | null>(null);
  let codeInput = $state('');
  // The row key whose inline revoke confirm is open.
  let revokeArmed = $state<string | null>(null);
  let destroyed = false;
  // True while a visit's own read is in flight: the machine still holds the
  // previous visit's snapshot until that read lands, so no fold may paint it.
  let reading = false;

  function fold(): void {
    if (!machine || destroyed || reading) return;
    const raw = machine.snapshotJson();
    if (!raw) return;
    const next = JSON.parse(raw) as ConnectedAppsSnapshot;
    snap = next;
    error = next.error ? resolveLocalized(next.error) : '';
  }

  onMount(async () => {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      const built = await createConnectedAppsMachine(
        { onChanged: () => fold() },
        await sharedRpcPort(id.secretHex),
      );
      if (destroyed) {
        built.free();
        return;
      }
      machine = built;
      await visit();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  });

  // A visit is a re-read: rows are nest state and a quiet push raises no event.
  // Drop the last visit's rows and drafts, and paint nothing until this read
  // returns. SvelteKit does not remount a route on a navigation to its own URL,
  // so every navigation — not only the mount — starts one.
  async function visit(): Promise<void> {
    if (!machine) return;
    reading = true;
    snap = null;
    revokeArmed = null;
    codeInput = '';
    error = '';
    try {
      await machine.refresh();
    } finally {
      reading = false;
    }
    fold();
  }

  afterNavigate(() => {
    if (machine) visit().catch((e) => { error = e instanceof Error ? e.message : String(e); });
  });

  onDestroy(() => {
    destroyed = true;
    machine?.free();
    machine = null;
  });

  async function submitCode(): Promise<void> {
    const code = codeInput.trim();
    if (!machine || !code) return;
    await machine.submitCode(code);
    codeInput = '';
    fold();
  }

  async function resolveRequest(request: ConsentCardRow, approved: boolean): Promise<void> {
    if (!machine) return;
    await machine.resolveRequest(request.consent_id_hex, approved);
    fold();
  }

  async function blockRequest(request: ConsentCardRow): Promise<void> {
    if (!machine) return;
    await machine.blockRequest(request.consent_id_hex);
    fold();
  }

  async function unblock(clientId: string): Promise<void> {
    if (!machine) return;
    await machine.unblock(clientId);
    fold();
  }

  async function confirmRevoke(key: string): Promise<void> {
    if (!machine) return;
    revokeArmed = null;
    await machine.revoke(key);
    fold();
  }

  // The class badge's words — the one per-app half of the grouping key, which the
  // machine derives. An unknown class paints no badge rather than a guess.
  function classLabel(cls: string): string | null {
    switch (cls) {
      case 'remote': return t.connected_apps.class_remote;
      case 'device': return t.connected_apps.class_device;
      case 'wasm': return t.connected_apps.class_wasm;
      case 'container': return t.connected_apps.class_container;
      case 'app_password': return t.connected_apps.class_app_password;
      case 'signer': return t.connected_apps.class_signer;
      case 'oauth': return t.connected_apps.class_oauth;
      default: return null;
    }
  }

  // Web formats times in JS: `format_unix_local` needs the OS timezone database,
  // which wasm lacks (same boundary as `DevicesSection`).
  function when(ms: number): string {
    return new Date(ms).toLocaleString();
  }

  function rowHead(row: ConnectedAppRow): string {
    const badge = classLabel(row.class);
    const name = resolveLocalized(row.name);
    return badge ? `${name} · ${badge}` : name;
  }

  // The row's description as ONE newline-joined text (the shape tui paints and
  // the cross-app actions read: name · badge / publisher — client id / one
  // "• scope" line each / the facts line). One text node, not a block per line:
  // the e2e text read is `textContent`, which has no break between blocks.
  function rowText(row: ConnectedAppRow): string {
    const lines = [rowHead(row)];
    if (row.client_id && row.publisher) {
      lines.push(`${t.connected_apps.publisher({ domain: row.publisher })} — ${row.client_id}`);
    }
    for (const scope of row.scope_descriptions) lines.push(`• ${resolveLocalized(scope)}`);
    lines.push(rowFacts(row));
    return lines.join('\n');
  }

  function rowFacts(row: ConnectedAppRow): string {
    const facts: string[] = [];
    if (!row.connected) facts.push(t.connected_apps.not_connected);
    facts.push(t.connected_apps.created({ time: when(row.created_at_millis) }));
    facts.push(
      row.last_used_at_millis != null
        ? t.connected_apps.last_used({ time: when(row.last_used_at_millis) })
        : t.connected_apps.never_used,
    );
    facts.push(
      row.lasts_until_millis != null
        ? t.connected_apps.lasts_until({ time: when(row.lasts_until_millis) })
        : t.connected_apps.open_ended,
    );
    return facts.join(' · ');
  }
</script>

<section class="section">
  <h2>{t.connected_apps.title}</h2>
  <p class="muted small">{t.connected_apps.description}</p>

  <!-- 1. Requests — only while a request is live. -->
  {#if (snap?.requests ?? []).length > 0}
    <h3>{t.connected_apps.requests_heading}</h3>
    {#each snap?.requests ?? [] as request (request.consent_id_hex)}
      <div class="card" data-testid={IDS.CONNECTED_APPS_REQUEST_CARD}>
        <p>{t.atproto_settings.consent_heading}</p>
        <p>
          {request.client_name
            ? t.atproto_settings.consent_client({ name: request.client_name, client_id: request.client_id })
            : t.atproto_settings.consent_client_unnamed({ client_id: request.client_id })}
        </p>
        <p>{t.atproto_settings.consent_scopes_heading}</p>
        <div class="scopes">
          {#each request.scope_descriptions as line}
            <div>• {line}</div>
          {/each}
        </div>
        {#each consentSetLines(request.sets) as setLine}
          <div class="scopes">{setLine}</div>
        {/each}
        <p data-testid={IDS.CONNECTED_APPS_REQUEST_CODE} data-code={request.code}>
          {t.atproto_settings.consent_code({ code: request.code })}
        </p>
        <p class="muted small">{t.atproto_settings.consent_code_hint}</p>
        <!-- Both controls render unconditionally — never gated on how close the
             request is to expiring: a resolution is reported even past expiry. -->
        <div class="actions">
          <button class="btn btn-primary" data-testid={IDS.CONNECTED_APPS_REQUEST_APPROVE} onclick={() => resolveRequest(request, true)}>
            {t.atproto_settings.consent_approve_button}
          </button>
          <button class="btn" data-testid={IDS.CONNECTED_APPS_REQUEST_DECLINE} onclick={() => resolveRequest(request, false)}>
            {t.atproto_settings.consent_deny_button}
          </button>
          <button class="btn" data-testid={IDS.CONNECTED_APPS_REQUEST_BLOCK} onclick={() => blockRequest(request)}>
            {t.connected_apps.block}
          </button>
        </div>
      </div>
    {/each}
  {/if}

  <!-- 2. Connect an app. -->
  <h3>{t.connected_apps.connect_heading}</h3>
  <p class="muted small">{t.connected_apps.connect_hint}</p>
  <div class="actions">
    <input
      class="input"
      data-testid={IDS.CONNECTED_APPS_CONNECT_CODE}
      placeholder={t.connected_apps.connect_placeholder}
      bind:value={codeInput}
      onkeydown={(e) => { if (e.key === 'Enter') submitCode(); }}
    />
    <button
      class="btn btn-primary"
      data-testid={IDS.CONNECTED_APPS_CONNECT_SUBMIT}
      disabled={!codeInput.trim()}
      onclick={submitCode}
    >{t.connected_apps.connect_submit}</button>
  </div>

  <!-- 3. The roster — the three-state list: nothing until the read returned,
       then rows or the empty state. -->
  <h3>{t.connected_apps.roster_heading}</h3>
  {#if snap?.loaded}
    {#if snap.principals.length === 0}
      <p class="muted small" data-testid={IDS.CONNECTED_APPS_EMPTY}>{t.connected_apps.empty}</p>
    {/if}
    {#each snap.principals as row (row.key)}
      <div class="card" data-testid={IDS.CONNECTED_APPS_ITEM} data-key={row.key}>
        <div class="row-text">{rowText(row)}</div>
        {#if revokeArmed === row.key}
          <p>{t.connected_apps.revoke_prompt({ name: resolveLocalized(row.name) })}</p>
          <div class="actions">
            <button class="btn danger small" data-testid={IDS.CONNECTED_APPS_ITEM_REVOKE_CONFIRM} onclick={() => confirmRevoke(row.key)}>
              {t.connected_apps.revoke_confirm}
            </button>
            <button class="btn small" data-testid={IDS.CONNECTED_APPS_ITEM_REVOKE_CANCEL} onclick={() => (revokeArmed = null)}>
              {t.connected_apps.revoke_cancel}
            </button>
          </div>
        {:else}
          <div class="actions">
            <button class="btn danger small" data-testid={IDS.CONNECTED_APPS_ITEM_REVOKE} onclick={() => (revokeArmed = row.key)}>
              {t.connected_apps.revoke}
            </button>
          </div>
        {/if}
      </div>
    {/each}
  {/if}

  <!-- 4. Blocked apps — only while something is blocked. -->
  {#if (snap?.blocked ?? []).length > 0}
    <h3>{t.connected_apps.blocked_heading}</h3>
    <p class="muted small">{t.connected_apps.blocked_hint}</p>
    {#each snap?.blocked ?? [] as blocked (blocked.client_id)}
      <div class="card" data-testid={IDS.CONNECTED_APPS_BLOCKED_ITEM}>
        <!-- The client id verbatim, as the request card showed it. -->
        <div>{blocked.client_id}</div>
        <div class="muted small">{t.connected_apps.blocked_since({ time: when(blocked.blocked_at_millis) })}</div>
        <div class="actions">
          <button class="btn small" data-testid={IDS.CONNECTED_APPS_BLOCKED_ITEM_UNBLOCK} onclick={() => unblock(blocked.client_id)}>
            {t.connected_apps.unblock}
          </button>
        </div>
      </div>
    {/each}
  {/if}
</section>

<style>
  .card {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.75rem;
    margin: 0.5rem 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .row-text { white-space: pre-line; }
  .actions { display: flex; gap: 0.5rem; flex-wrap: wrap; align-items: center; margin-top: 0.25rem; }
  .scopes { display: flex; flex-direction: column; }
  h3 { margin-top: 1.25rem; font-size: 1rem; }
</style>
