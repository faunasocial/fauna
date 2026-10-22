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
  // `#lib/wasm-connected-apps`): roster composition, scope words, the class key,
  // *lasts-until* and which verb revokes a row are the machine's, so this file
  // never picks a revoke verb — a row's `key` is opaque. Reference painter:
  // `apps/fauna-tui/src/settings/connected_apps.rs`.
  //
  // The lift: the AT Protocol page's consent cards + connected-app rows, the
  // Nostr page's bunker rows and Mail & Calendar's app-password rows render HERE
  // and no longer on their old pages. The mail rows ride a mail-settings machine
  // the chunk builds for itself (`#lib/wasm-connected-apps`); a mail row alone
  // carries the type / login / secret leaves, and its secret is read on demand
  // (`revealSecret`), never part of the snapshot.
  import { onDestroy, onMount } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import { identity } from '#lib/store';
  import { sharedRpcPort } from '#lib/rpc';
  import { resolveComposed, resolveLocalized } from '#lib/i18n/localized';
  import {
    composeRequestCard,
    composeRequestInstall,
    composeRosterRow,
    createConnectedAppsMachine,
    factsSeparator,
    lineSeparator,
    type ConnectedAppRow,
    type ConnectedAppsMachine,
    type ConnectedAppsSnapshot,
    type ConsentCardRow,
  } from '#lib/wasm-connected-apps';
  import { ensureWasm, resolveMuaUsername } from '#lib/wasm';
  import { t } from '#lib/i18n/strings';
  import { localTimestamp } from '#lib/value-format';
  import { IDS } from '#lib/generated/uiIds';

  // The parent settings page owns the single error surface (`error-message`).
  let { error = $bindable('') } = $props();

  let machine: ConnectedAppsMachine | null = null;
  // `null` until the first fold: a visit paints neither rows nor the empty state
  // until its own read has returned (`connected-apps.md` § Errors & edge cases).
  let snap = $state<ConnectedAppsSnapshot | null>(null);
  let codeInput = $state('');
  // The row key whose inline revoke confirm is open.
  let revokeArmed = $state<string | null>(null);
  // Mail app-password secrets the user revealed, by row key. Held only while
  // shown and dropped on every visit; never logged or persisted.
  let revealed = $state<Record<string, string>>({});
  // The login the copy button last put on the clipboard, published as its
  // `copied` attr (the `account-actor-id-copy-btn` contract), by row key.
  let copiedLogin = $state<{ key: string; login: string } | null>(null);
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
      // The core chunk resolves a mail row's login (`resolveMuaUsername`).
      await ensureWasm();
      const built = await createConnectedAppsMachine(
        { onChanged: () => fold() },
        await sharedRpcPort(id.secretHex),
        id.secretHex,
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
    revealed = {};
    copiedLogin = null;
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

  // Non-optimistic: the machine writes, re-reads, and the toggle paints the
  // re-read's `external_apps_off`, never a local guess.
  async function toggleExternalApps(): Promise<void> {
    if (!machine) return;
    await machine.setExternalAppsEnabled(!!snap?.external_apps_off);
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

  // Reveal is a toggle: the first click reads the secret on demand, the second
  // hides it again. A failed read is the machine's page error (`null` here).
  async function toggleSecret(key: string): Promise<void> {
    if (key in revealed) {
      const next = { ...revealed };
      delete next[key];
      revealed = next;
      return;
    }
    if (!machine) return;
    const secret = await machine.revealSecret(key);
    if (secret != null) revealed = { ...revealed, [key]: secret };
    fold();
  }

  async function copyText(value: string): Promise<void> {
    try { await navigator.clipboard.writeText(value); } catch { /* headless / no perm */ }
  }

  async function copyLogin(key: string, login: string): Promise<void> {
    await copyText(login);
    copiedLogin = { key, login };
  }

  // The concrete login: shared Rust resolved everything but `{handle}`, so the
  // paint is one substitution and never a locally built address.
  function mailLogin(template: string): string {
    return resolveMuaUsername(template, $identity?.handle ?? '');
  }

  // The row's description as ONE newline-joined text (the shape tui paints and
  // the cross-app actions read), composed once in shared Rust
  // (`compose::roster_row_text`): name · badge / a burned row's revoked line /
  // publisher — client id / the scopes / the permission sets / the oracle
  // summary / the facts line. Web resolves each `ComposedText` through its own
  // string table and joins; instants are formatted by `localTimestamp`
  // (`#lib/value-format`, `format_unix_local_ms`'s shape — wasm has no timezone
  // database). One text node, not a block per line: the e2e text read is
  // `textContent`, which has no break between blocks.
  function rowText(row: ConnectedAppRow): string {
    const text = composeRosterRow(row);
    const facts = text.facts.map(resolveComposed).join(factsSeparator());
    return [...text.lines.map(resolveComposed), facts].join('\n');
  }

  // Whether the row links to its plugin's settings card on the Bridges page —
  // the shared composition decides (`opens_settings_card`).
  function opensSettingsCard(row: ConnectedAppRow): boolean {
    return composeRosterRow(row).opens_settings_card;
  }
</script>

<section class="section">
  <h2>{t.connected_apps.title}</h2>
  <p class="muted small">{t.connected_apps.description}</p>

  <!-- 1. Requests — only while a request is live. -->
  {#if (snap?.requests ?? []).length > 0}
    <h3>{t.connected_apps.requests_heading}</h3>
    {#each snap?.requests ?? [] as request (request.consent_id_hex)}
      {@const install = composeRequestInstall(request)}
      <div class="card" data-testid={IDS.CONNECTED_APPS_REQUEST_CARD}>
        <!-- The card's lines are composed once in shared Rust
             (`compose::request_card_lines`): heading, who is asking, the scopes
             heading and its bullets, the permission sets, then what the approve
             ends besides granting. Only the markup ramp is the shell's. -->
        {#each composeRequestCard(request).map(resolveComposed) as line, i}
          {#if i < 3}<p>{line}</p>{:else}<div class="scopes">{line}</div>{/if}
        {/each}
        <!-- An admin's hosted-plugin install card: what approving would run on
             the nest, one leaf per fact (`compose::request_install_text`), between
             the scopes and the binding code. -->
        {#if install}
          <h4 data-testid={IDS.CONNECTED_APPS_REQUEST_INSTALL}>{resolveComposed(install.heading)}</h4>
          <p data-testid={IDS.CONNECTED_APPS_REQUEST_INSTALL_FORM} data-form={request.install?.execution_form ?? ''}>{resolveComposed(install.form)}</p>
          <p data-testid={IDS.CONNECTED_APPS_REQUEST_INSTALL_PUBLISHER}>{resolveComposed(install.publisher)}</p>
          <p data-testid={IDS.CONNECTED_APPS_REQUEST_INSTALL_DIGEST}>{resolveComposed(install.digest)}</p>
          <div class="scopes" data-testid={IDS.CONNECTED_APPS_REQUEST_INSTALL_KINDS}>{install.kinds.map(resolveComposed).join(lineSeparator())}</div>
          <div class="scopes" data-testid={IDS.CONNECTED_APPS_REQUEST_INSTALL_HOSTS}>{install.hosts.map(resolveComposed).join(lineSeparator())}</div>
          <div class="scopes" data-testid={IDS.CONNECTED_APPS_REQUEST_INSTALL_SETTINGS}>{install.settings.map(resolveComposed).join(lineSeparator())}</div>
        {/if}
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

  <!-- 3. The external-apps switch, then the roster it pauses — at every
       integration level (the flag is the account's). An absent snapshot paints
       the flag's own default, ON. The hint carries no id (ui.yaml gives it none). -->
  <button
    class="btn toggle"
    class:on={!snap?.external_apps_off}
    data-testid={IDS.ATPROTO_EXTERNAL_APPS_ENABLE}
    data-state={snap?.external_apps_off ? 'off' : 'on'}
    onclick={toggleExternalApps}
  >{t.atproto_settings.external_apps_toggle}</button>
  <p class="muted small">{t.atproto_settings.external_apps_hint}</p>

  <!-- The roster — the three-state list: nothing until the read returned,
       then rows or the empty state. -->
  <h3>{t.connected_apps.roster_heading}</h3>
  {#if snap?.loaded}
    {#if snap.principals.length === 0}
      <p class="muted small" data-testid={IDS.CONNECTED_APPS_EMPTY}>{t.connected_apps.empty}</p>
    {/if}
    {#each snap.principals as row (row.key)}
      <div
        class="card"
        data-testid={IDS.CONNECTED_APPS_ITEM}
        data-key={row.key}
        data-revoked={row.mail?.revoked || row.signer_burned ? 'true' : undefined}
        data-oracle_ops={row.oracle ? String(row.oracle.ops_in_window) : undefined}
        data-status={row.paused ? 'paused' : row.connected ? 'connected' : 'not_connected'}
      >
        <div class="row-text">{rowText(row)}</div>
        {#if opensSettingsCard(row)}
          <a class="btn small" href="/app/bridges" data-testid={IDS.CONNECTED_APPS_ITEM_SETTINGS_LINK}>{t.connected_apps.settings_link}</a>
        {/if}
        {#if row.mail}
          {@const login = mailLogin(row.mail.mua_username)}
          <span class="badge" data-testid={IDS.CONNECTED_APPS_ITEM_TYPE}>{resolveLocalized(row.mail.kind)}</span>
          <div class="field">
            <span class="field-label">{t.settings.mail.mua_username}</span>
            <code class="mono" data-testid={IDS.CONNECTED_APPS_ITEM_USERNAME}>{login}</code>
            <button
              class="btn small"
              data-testid={IDS.CONNECTED_APPS_ITEM_COPY_USERNAME}
              data-copied={copiedLogin?.key === row.key ? copiedLogin.login : undefined}
              onclick={() => copyLogin(row.key, login)}
            >{t.settings.mail.copy_username}</button>
          </div>
          <!-- ⚠ The hidden secret's text MUST stay EMPTY — never a mask: the
               cross-app driver polls it until it turns non-empty and returns
               that AS the secret, so a mask would pass the reveal test without
               the on-demand read ever running. -->
          <div class="field">
            <span class="field-label">{t.settings.mail.secret_label}</span>
            <code class="mono" data-testid={IDS.CONNECTED_APPS_ITEM_SECRET}>{revealed[row.key] ?? ''}</code>
            <button class="btn small" data-testid={IDS.CONNECTED_APPS_ITEM_REVEAL_SECRET} onclick={() => toggleSecret(row.key)}>
              {row.key in revealed ? t.settings.mail.hide_secret : t.settings.mail.reveal_secret}
            </button>
            <button class="btn small" data-testid={IDS.CONNECTED_APPS_ITEM_COPY_SECRET} onclick={() => copyText(revealed[row.key] ?? '')}>
              {t.settings.mail.copy_secret}
            </button>
          </div>
        {/if}
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
        <div class="muted small">{t.connected_apps.blocked_since({ time: localTimestamp(blocked.blocked_at_millis) })}</div>
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
  .badge { align-self: flex-start; font-size: 0.8rem; border: 1px solid var(--border); border-radius: 4px; padding: 0 0.4rem; }
  .field { display: flex; gap: 0.5rem; flex-wrap: wrap; align-items: center; }
  .field-label { font-size: 0.85rem; }
  .mono { font-family: monospace; }
  .actions { display: flex; gap: 0.5rem; flex-wrap: wrap; align-items: center; margin-top: 0.25rem; }
  .scopes { display: flex; flex-direction: column; }
  h3 { margin-top: 1.25rem; font-size: 1rem; }
</style>
