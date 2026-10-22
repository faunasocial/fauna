<script lang="ts">
  // `nest_retire` — retiring an app-provisioned nest
  // (docs/goal/behavior/nest-retirement.md, the owner of every rule below).
  //
  // A paint shell over the shared `NestRetireMachine`, hosted by the onboarding
  // route's wizard host exactly as `nest_recovery` is — including over a live
  // session. ui.yaml `onboarding.nest_retire`. States, each the machine's:
  // credentials (the `vps_config` provider row, credential form and verify
  // button, reused by id — no region or server-type controls) → list
  // (`retire-server-item`, its members scoped inside the row) → confirm (the
  // typed-name gate) → running (two `provisioning-step-row` rows: DNS, then the
  // server) → done (the by-hand list).
  //
  // **Nothing here decides anything.** The typed-name gate and the binding of a
  // run to the confirmed server are the machine's (`confirm_enabled`,
  // `force_server_offered`); every sentence that names what dies or what is
  // left is a machine fold (`confirmSummaryJson`, `leftoverLinesJson`,
  // `transferCodeNoteJson`, `stepLabelJson`, `stepNoteJson`); the credential
  // fields and the verify gate are the machine's (`visibleFieldsJson`,
  // `canVerify`). The VPS token lives in the machine for the page's lifetime
  // and is dropped on exit (`cancel`). Reference app: tui
  // (`apps/fauna-tui/src/wizard/nest_retire.rs`).
  import { onDestroy, onMount } from 'svelte';
  import { IDS } from '#lib/generated/uiIds';
  import { t } from '#lib/i18n/strings';
  import {
    resolveKey,
    resolveLocalized,
    resolveLocalizedNested,
    type LocalizedText,
  } from '#lib/i18n/localized';
  import { PROVIDERS } from '#lib/generated/providers';
  import { localDomainMachine } from '#lib/rpc';
  import { ensureWasm, logMessage, retireHeldDns } from '#lib/wasm';
  import {
    createNestRetireMachine,
    provisioningStatusGlyph,
    type NestRetireMachine,
    type RetireField,
    type RetireServerRow,
    type RetireSnapshot,
    type RetireStepState,
  } from '#lib/wasm-onboarding';
  import { pendingProviderBaseUrl } from '#lib/onboarding/machine.svelte';

  interface Props {
    /** Which entry opened the page — where Back and Done return to. */
    entry: 'launch' | 'admin';
    /** The saved nest's URL: its host is an attribution candidate and, once
     *  resolved, the *current* badge's address. */
    nestUrl: string | null;
    /** The signed-in actor's secret — the admin entry's local-domain read. */
    secretHex: string | null;
    /** Leave the page; `retiredCurrent` when the run destroyed the box this
     *  session is signed into. */
    onclose: (retiredCurrent: boolean) => void;
  }

  let { entry, nestUrl, secretHex, onclose }: Props = $props();

  /** How long the live input reads may hold verify before the page goes on
   *  with what it opened with (tui's `ADMIN_REFRESH_BOUND`). */
  const APP_INPUTS_BOUND_MS = 15_000;

  let m: NestRetireMachine | null = $state(null);
  // Bumped after every machine call, so every read below re-runs.
  let tick = $state(0);
  let busy = $state(0);
  // Paint buffers for what was typed — the machine holds the token as a secret
  // and hands nothing back.
  let creds: Record<string, string> = $state({});
  let confirmTyped = $state('');
  let poll: ReturnType<typeof setInterval> | null = null;

  const refresh = () => { tick += 1; };

  const snap: RetireSnapshot | null = $derived.by(() => {
    void tick;
    return m ? (JSON.parse(m.snapshotJson()) as RetireSnapshot) : null;
  });
  const selectedProvider: string | undefined = $derived.by(() => {
    void tick;
    return m?.selectedProvider() ?? undefined;
  });
  const fields: RetireField[] = $derived.by(() => {
    void tick;
    return m ? (JSON.parse(m.visibleFieldsJson()) as RetireField[]) : [];
  });
  // Each `hosted-auth` field's sign-in button — its label and whether it can
  // be pressed are the machine's.
  const hostedAuthView: Record<string, { label: string; enabled: boolean }> = $derived.by(() => {
    void tick;
    const out: Record<string, { label: string; enabled: boolean }> = {};
    if (!m) return out;
    for (const f of fields) {
      if (f.field_type !== 'HostedAuth') continue;
      out[f.id] = { label: m.hostedAuthButtonText(f.id), enabled: m.hostedAuthCanBegin(f.id) };
    }
    return out;
  });
  const canVerify: boolean = $derived.by(() => {
    void tick;
    return m?.canVerify() ?? false;
  });
  const selectedRow: RetireServerRow | undefined = $derived(
    snap?.servers.find((r) => r.server_id === snap?.selected),
  );
  const runFailed: boolean = $derived(
    snap?.steps.some((s) => typeof s.state === 'object' && 'Failed' in s.state) ?? false,
  );
  const leftoverText: string = $derived.by(() => {
    void tick;
    if (!m) return '';
    return (JSON.parse(m.leftoverLinesJson()) as LocalizedText[])
      .map((l) => resolveLocalized(l))
      .join('\n');
  });
  // Every VPS-capable provider — the listing primitive is on all six adapters,
  // and a box bought through a provider no longer on the curated offer list
  // must still be retirable.
  const vpsProviders = PROVIDERS.filter((p) => p.capabilities.includes('vps'));

  /** Run one machine call, repainting while it is in flight (the run's steps
   *  advance inside `confirm`). */
  async function act(op: () => Promise<unknown> | void) {
    busy += 1;
    if (poll === null) poll = setInterval(refresh, 250);
    try {
      await op();
    } catch (e) {
      logMessage('warn', 'fauna_web::nest_retire', `retire action failed: ${e}`);
    } finally {
      busy -= 1;
      if (busy === 0 && poll !== null) {
        clearInterval(poll);
        poll = null;
      }
      refresh();
    }
  }

  function nestHost(): string | null {
    if (!nestUrl) return null;
    try {
      return new URL(nestUrl).hostname.replace(/^\[|\]$/g, '') || null;
    } catch {
      return null;
    }
  }

  onMount(async () => {
    await ensureWasm();
    const host = nestHost();
    // A host name (never an address) is attribution candidate (2).
    const isAddress = host !== null && (/^[\d.]+$/.test(host) || host.includes(':'));
    const machine = await createNestRetireMachine(
      host && !isAddress ? [host] : [],
      pendingProviderBaseUrl('vps'),
      pendingProviderBaseUrl('doh'),
    );
    m = machine;
    refresh();
    // The live input reads (§ Credential stance (b), § DNS cleanup → *Several
    // domains* (1)): every held DNS credential and the remembered slot names
    // from the account's DNS record whenever this tab's account runtime is up,
    // and — from `admin-nest` — the nest's active local domains. The machine
    // holds verify until they land; best-effort and bounded.
    machine.expectAppInputs();
    refresh();
    const reads = (async () => {
      if (entry === 'admin' && secretHex) {
        try {
          const ld = await localDomainMachine(secretHex);
          await ld.hydrate();
          const active = ((ld.snapshot() as { active?: { domain: string }[] } | null)?.active ?? [])
            .map((d) => d.domain);
          machine.addCandidateDomains(active);
        } catch (e) {
          logMessage('info', 'fauna_web::nest_retire', `local-domain read failed (best-effort): ${e}`);
        }
      }
      try {
        const held = await retireHeldDns();
        if (held && 'error' in held) {
          logMessage('info', 'fauna_web::nest_retire', `DNS record read failed (best-effort): ${held.error}`);
          // A first-listing refusal is not "the account holds none": say why
          // the held credentials could not be read.
          if (held.notReady) machine.setInputError(held.error);
        } else if (held) {
          machine.setHeldDnsJson(held.providerIds, held.credsJson);
          // The remembered DKIM / `_atproto` / `_fauna` names ride in with the
          // same record (read-only for the machine).
          machine.setRememberedSlotsJson(held.dkim, held.atproto, held.faunaSelf);
        }
      } catch (e) {
        logMessage('info', 'fauna_web::nest_retire', `DNS record read failed (best-effort): ${e}`);
      }
    })();
    const bound = new Promise<void>((resolve) => setTimeout(resolve, APP_INPUTS_BOUND_MS));
    await Promise.race([reads, bound]);
    machine.appInputsLanded();
    refresh();
    // The *current* badge — the browser has no resolver, so the machine asks
    // the public one. An unresolvable host badges nothing.
    if (host) void machine.resolveCurrentHost(host).then(refresh, () => {});
  });

  onDestroy(() => {
    if (poll !== null) clearInterval(poll);
    // Drop the token and everything fetched with it.
    m?.cancel();
    m?.free();
    m = null;
  });

  function close() {
    const retiredCurrent = snap?.retired?.current ?? false;
    onclose(retiredCurrent);
  }

  function selectProvider(id: string) {
    if (!m) return;
    if (selectedProvider !== id) creds = {};
    m.selectProvider(id);
    refresh();
  }

  function setCred(id: string, value: string) {
    if (!m) return;
    creds[id] = value;
    m.setCredentialField(id, value);
    refresh();
  }

  async function hostedAuth(fieldId: string) {
    if (!m) return;
    const machine = m;
    // Open the tab inside the gesture so popup blockers let it through, then
    // point it at the verification URL once the request lands (the wizard's
    // `beginHostedAuth` shape).
    const tab = window.open('about:blank', '_blank', 'noopener,noreferrer');
    await act(async () => {
      const raw = await machine.hostedAuthBegin(fieldId);
      refresh();
      if (raw) {
        const url = (JSON.parse(raw) as { verification_url: string }).verification_url;
        if (tab && !tab.closed) tab.location.href = url;
        else window.open(url, '_blank', 'noopener,noreferrer');
        await machine.hostedAuthWait(fieldId);
      } else if (tab && !tab.closed) {
        tab.close();
      }
    });
  }

  function back() {
    if (!m || !snap) return;
    switch (snap.phase) {
      // Credentials — and a run that stopped on a failure — leave the page;
      // `cancel` (onDestroy) disarms it, so nothing re-targets the run.
      case 'Credentials':
      case 'Running':
      case 'Done':
        close();
        return;
      case 'List':
      case 'Confirm':
        m.back();
        confirmTyped = '';
        refresh();
        return;
      default:
        return;
    }
  }

  function stepStatus(state: RetireStepState): string {
    if (!m) return '';
    return JSON.parse(m.stepStatusJson(JSON.stringify(state))) as string;
  }

  function stepNote(state: RetireStepState): string {
    void tick;
    if (!m) return '';
    const note = JSON.parse(m.stepNoteJson(JSON.stringify(state))) as LocalizedText | null;
    return note ? resolveLocalized(note) : '';
  }

  function stepLabel(step: string): string {
    if (!m) return '';
    return resolveLocalized(JSON.parse(m.stepLabelJson(step)) as LocalizedText);
  }

  function transferNote(serverId: string): string {
    void tick;
    if (!m) return '';
    const note = JSON.parse(m.transferCodeNoteJson(serverId)) as LocalizedText | null;
    return note ? resolveLocalized(note) : '';
  }

  const confirmSummary: string = $derived.by(() => {
    void tick;
    if (!m || snap?.phase !== 'Confirm') return '';
    // The provider rides as its display KEY, so it is resolved too.
    return (JSON.parse(m.confirmSummaryJson()) as LocalizedText[])
      .map((l) => resolveLocalizedNested(l))
      .join('\n');
  });
</script>

<div class="center">
  <h1>{t.onboarding.retire.title}</h1>
  <p class="muted">{t.onboarding.retire.subtitle}</p>

  {#if snap?.error}
    <div data-testid={IDS.ERROR_MESSAGE} class="error">{snap.error}</div>
  {/if}

  {#if snap && (snap.phase === 'Credentials' || snap.phase === 'Listing')}
    <div data-testid={IDS.VPS_PROVIDER_ROW} class="provider-row">
      {#each vpsProviders as p (p.id)}
        <button
          data-testid={`vps-provider-row[${p.id}]`}
          class="btn"
          class:selected={selectedProvider === p.id}
          onclick={() => selectProvider(p.id)}
        >{resolveKey(p.displayNameKey)}</button>
        <!-- Rendered inside the {#each}, like vps_config's: a step-level {#if}
             off the selection is not re-scheduled reliably in Svelte 5. -->
        {#if selectedProvider === p.id}
          <div class="provider-detail">
            <a
              data-testid={IDS.VPS_PROVIDER_LINK}
              href={p.signupUrl}
              target="_blank"
              rel="noopener noreferrer"
            >{p.signupUrl}</a>
            <button
              class="btn"
              data-testid={IDS.VPS_PROVIDER_OPEN_BROWSER_BUTTON}
              onclick={() => window.open(p.signupUrl, '_blank', 'noopener,noreferrer')}
            >{t.onboarding.dns_config.open_in_browser}</button>
            <p data-testid={IDS.VPS_PROVIDER_HELP_TEXT}>{resolveKey(p.helpKey)}</p>

            <div data-testid={IDS.VPS_CREDENTIALS_FORM}>
              {#each fields as field (field.id)}
                {#if field.field_type === 'HostedAuth'}
                  <div class="form-group">
                    <span class="field-label">{resolveKey(field.label_key)}</span>
                    <button
                      class="btn"
                      data-testid={`vps-credentials-form-${field.id}`}
                      disabled={!(hostedAuthView[field.id]?.enabled ?? false)}
                      onclick={() => hostedAuth(field.id)}
                    >{hostedAuthView[field.id]?.label ?? ''}</button>
                  </div>
                {:else}
                  <label class="form-group">
                    <span class="field-label">{resolveKey(field.label_key)}</span>
                    <input
                      type={field.field_type === 'Secret' ? 'password' : 'text'}
                      class="input"
                      data-testid={`vps-credentials-form-${field.id}`}
                      value={creds[field.id] ?? ''}
                      oninput={(e) => setCred(field.id, (e.currentTarget as HTMLInputElement).value)}
                    />
                  </label>
                {/if}
              {/each}
            </div>

            <button
              class="btn primary"
              data-testid={IDS.VPS_VERIFY_BUTTON}
              disabled={!canVerify || snap.phase !== 'Credentials'}
              onclick={() => act(() => m?.verify())}
            >{snap.phase === 'Listing' ? t.common.loading : t.provisioning.verify_credentials}</button>
          </div>
        {/if}
      {/each}
    </div>
    <div class="button-group">
      <button class="btn link" data-testid={IDS.RETIRE_BACK_BUTTON} onclick={back}>{t.common.back}</button>
    </div>

  {:else if snap && snap.phase === 'List'}
    {#if snap.servers.length === 0}
      <p data-testid={IDS.RETIRE_SERVER_EMPTY_MESSAGE} class="muted">{t.onboarding.retire.empty_message}</p>
    {:else}
      <div data-testid={IDS.RETIRE_SERVER_LIST} class="retire-list">
        <h2>{t.onboarding.retire.list_label}</h2>
        {#each snap.servers as row (row.server_id)}
          {@const isSelected = snap.selected === row.server_id}
          <div
            data-testid={IDS.RETIRE_SERVER_ITEM}
            class="retire-row"
            class:selected={isSelected}
            role="button"
            tabindex="0"
            aria-pressed={isSelected}
            onclick={() => { m?.select(row.server_id); refresh(); }}
            onkeydown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); m?.select(row.server_id); refresh(); }
            }}
          >
            <strong data-testid={IDS.RETIRE_SERVER_NAME}>{row.name}</strong>
            {#if row.domain}
              <span class="retire-field">{t.onboarding.retire.domain_label}:
                <span data-testid={IDS.RETIRE_SERVER_DOMAIN}>{row.domain}</span></span>
            {/if}
            {#if row.secondary_domains.length > 0}
              <span class="retire-field">{t.onboarding.retire.secondary_domains_label}:
                <span data-testid={IDS.RETIRE_SERVER_SECONDARY_DOMAINS}>{row.secondary_domains.join(', ')}</span></span>
            {/if}
            {#if row.ipv4}
              <span class="retire-field">{t.onboarding.retire.address_label}:
                <span data-testid={IDS.RETIRE_SERVER_ADDRESS}>{row.ipv4}</span></span>
            {/if}
            {#if row.current}
              <span data-testid={IDS.RETIRE_SERVER_CURRENT_BADGE} class="badge">{t.onboarding.retire.current_badge}</span>
            {/if}
            {#if !row.marked}
              <span data-testid={IDS.RETIRE_SERVER_UNMARKED_NOTE} class="muted">{t.onboarding.retire.unmarked_note}</span>
            {/if}
            <!-- The transfer-code affordance sits on the selected row and is
                 independent of deleting. -->
            {#if isSelected}
              {#if row.transfer_code === 'Idle'}
                <button
                  class="btn"
                  data-testid={IDS.RETIRE_TRANSFER_CODE_BUTTON}
                  onclick={(e) => { e.stopPropagation(); void act(() => m?.fetchTransferCode()); }}
                >{t.onboarding.retire.transfer_code_button}</button>
              {:else if typeof row.transfer_code === 'object' && 'Code' in row.transfer_code}
                {@const code = row.transfer_code.Code.code}
                <span class="retire-field">{t.onboarding.retire.transfer_code_label}:
                  <code data-testid={IDS.RETIRE_TRANSFER_CODE_VALUE}>{code}</code></span>
                <button
                  class="btn"
                  data-testid={IDS.RETIRE_TRANSFER_CODE_COPY_BUTTON}
                  onclick={(e) => { e.stopPropagation(); void navigator.clipboard?.writeText(code); }}
                >{t.onboarding.retire.transfer_code_copy}</button>
              {/if}
              {@const note = transferNote(row.server_id)}
              {#if note}
                <span data-testid={IDS.RETIRE_TRANSFER_CODE_STATUS} class="muted">{note}</span>
              {/if}
            {/if}
          </div>
        {/each}
      </div>
    {/if}
    <div class="button-group">
      <button
        class="btn danger"
        data-testid={IDS.RETIRE_DELETE_BUTTON}
        disabled={!selectedRow}
        onclick={() => { confirmTyped = ''; m?.beginConfirm(); refresh(); }}
      >{t.onboarding.retire.delete_button}</button>
      <button class="btn link" data-testid={IDS.RETIRE_BACK_BUTTON} onclick={back}>{t.common.back}</button>
    </div>

  {:else if snap && snap.phase === 'Confirm'}
    <!-- The typed-name gate (nest-retirement.md § Confirm shape). -->
    <p data-testid={IDS.RETIRE_CONFIRM_SUMMARY} class="retire-summary">{confirmSummary}</p>
    <label class="form-group">
      <span class="field-label">{t.onboarding.retire.confirm_name_label}</span>
      <input
        type="text"
        class="input"
        data-testid={IDS.RETIRE_CONFIRM_NAME_INPUT}
        value={confirmTyped}
        oninput={(e) => {
          confirmTyped = (e.currentTarget as HTMLInputElement).value;
          m?.setConfirmName(confirmTyped);
          refresh();
        }}
      />
    </label>
    <div class="button-group">
      <button
        class="btn danger"
        data-testid={IDS.RETIRE_CONFIRM_BUTTON}
        disabled={!snap.confirm_enabled || busy > 0}
        onclick={() => act(() => m?.confirm())}
      >{t.onboarding.retire.confirm_button}</button>
      <button
        class="btn link"
        data-testid={IDS.RETIRE_CANCEL_BUTTON}
        onclick={() => { confirmTyped = ''; m?.back(); refresh(); }}
      >{t.onboarding.retire.cancel_button}</button>
    </div>

  {:else if snap && (snap.phase === 'Running' || snap.phase === 'Done')}
    {#if snap.phase === 'Done' && snap.retired}
      <p>{t.onboarding.retire.done_deleted({ name: snap.retired.name })}</p>
    {/if}
    <!-- The two step rows, reusing the provisioning-progress row ids and its
         glyph vocabulary (0 = DNS records, 1 = the server). -->
    <div class="prov-rows">
      {#each snap.steps as s (s.step)}
        {@const note = stepNote(s.state)}
        <div data-testid={IDS.PROVISIONING_STEP_ROW} class="prov-row">
          <span data-testid={IDS.PROVISIONING_STEP_CHECKBOX} class="prov-checkbox" aria-hidden="true">
            {provisioningStatusGlyph(stepStatus(s.state))}
          </span>
          <span data-testid={IDS.PROVISIONING_STEP_LABEL} class="prov-label">{stepLabel(s.step)}</span>
          {#if note}
            <span data-testid={IDS.PROVISIONING_SUBSTEP} class="prov-substep">{note}</span>
          {/if}
          {#if typeof s.state === 'object' && 'Failed' in s.state}
            <span data-testid={IDS.PROVISIONING_STEP_ERROR} class="prov-error">{s.state.Failed.cause}</span>
          {/if}
        </div>
      {/each}
    </div>

    {#if snap.phase === 'Running'}
      <div class="button-group">
        {#if runFailed}
          <button
            class="btn"
            data-testid={IDS.RETIRE_RETRY_BUTTON}
            disabled={busy > 0}
            onclick={() => act(() => m?.retry())}
          >{t.onboarding.retire.retry_button}</button>
        {/if}
        {#if snap.force_server_offered}
          <!-- The dangling-record warning, repeated where the choice is made. -->
          <p class="muted" data-testid={IDS.RETIRE_FORCE_SERVER_WARNING}>{t.onboarding.retire.force_server_warning}</p>
          <button
            class="btn danger"
            data-testid={IDS.RETIRE_FORCE_SERVER_BUTTON}
            disabled={busy > 0}
            onclick={() => act(() => m?.forceServer())}
          >{t.onboarding.retire.force_server_button}</button>
        {/if}
        <!-- Leaving is offered only once the run has stopped on a failure —
             never while a step is still in flight. -->
        {#if runFailed}
          <button class="btn link" data-testid={IDS.RETIRE_BACK_BUTTON} onclick={back}>{t.common.back}</button>
        {/if}
      </div>
    {:else}
      <pre data-testid={IDS.RETIRE_DNS_LEFTOVER_TEXT} class="dns-records">{leftoverText}</pre>
      <div class="button-group">
        <button
          class="btn"
          data-testid={IDS.RETIRE_DNS_LEFTOVER_COPY_BUTTON}
          onclick={() => { void navigator.clipboard?.writeText(leftoverText); }}
        >{t.onboarding.retire.leftover_copy}</button>
        <button
          class="btn primary"
          data-testid={IDS.RETIRE_DONE_BUTTON}
          onclick={close}
        >{t.onboarding.retire.done_button}</button>
      </div>
    {/if}
  {:else}
    <p class="muted">{t.common.loading}</p>
    <div class="button-group">
      <button class="btn link" data-testid={IDS.RETIRE_BACK_BUTTON} onclick={() => onclose(false)}>{t.common.back}</button>
    </div>
  {/if}
</div>

<style>
  .retire-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    width: 100%;
  }
  .retire-row {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 0.75rem;
    border: 1px solid var(--border, #ccc);
    border-radius: 6px;
    cursor: pointer;
    text-align: left;
  }
  .retire-row.selected {
    border-color: var(--accent, #4a7);
    box-shadow: 0 0 0 1px var(--accent, #4a7);
  }
  .retire-summary {
    white-space: pre-line;
  }
  .badge {
    font-size: 0.85em;
    font-weight: 600;
  }
</style>
