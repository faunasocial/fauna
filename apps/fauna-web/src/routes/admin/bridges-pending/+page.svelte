<script lang="ts">
  // Admin pending-bridge approval page (`admin-bridges-pending`): a flat admin
  // mail pane (memory mail-ux-flat-not-bridges-detail) listing mail bridges that
  // have enrolled and await the admin's approval. The admin verifies each
  // bridge's Ed25519 pubkey against the admin's expected fingerprint, then
  // approves (applies the per-role allowlist) or rejects it.
  //
  // A dumb renderer of the shared `BridgeApprovalMachine`
  // (libs/fauna-client-mail-settings::bridge_approval, WASM twin via
  // bridgeApprovalMachine()): build over the singleton WS-RPC client → hydrate()
  // → render snapshot() → dispatch(action) → re-render. No approval logic in the
  // SPA (priority #2). Lifts the linux standalone sub-page
  // (apps/fauna-linux/src/views/admin.rs::build_pending_bridge_card). The backend
  // is real (fauna.bridges.{list_pending,approve,reject}), so the page is green.
  // The machine auto-refreshes after Approve/Reject, so the page just re-reads
  // snapshot() after each dispatch. The deployment mail.enabled toggle is NOT on
  // this page (onboarding + mail-settings own it). UX/IDs: tests/e2e-unified/
  // ui.yaml `admin-bridges-pending` page + `admin-bridges-pending-card` component.
  import { identity } from '$lib/store';
  import { bridgeApprovalMachine } from '$lib/rpc';
  import { ensureWasm, bridgeDisplayName } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { onMount } from 'svelte';
  import { afterNavigate } from '$app/navigation';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // Shapes mirror the shared serde JSON (snake_case across serde_wasm_bindgen;
  // the status enum serializes as its variant-name string).
  interface PendingBridgeView {
    pubkey_hex: string;
    requested_role: string;
    source_ip: string | null;
    first_seen_at: number;
  }
  // Approved (running-phase) bridge roster row — carries the rotate-service-user-key
  // affordance (admin.md § Approved-bridges roster; mail-bridge-lifecycle.md
  // § Service-user re-keying).
  interface ApprovedBridgeView {
    pubkey_hex: string;
    role: string;
    approved_at: number | null;
  }
  interface BridgeApprovalSnapshot {
    pending: PendingBridgeView[];
    approved: ApprovedBridgeView[];
    mail_enabled: boolean | null;
    status: string; // "Idle" | "Loading" | "Working"
    error: string | null;
  }

  let machine: Awaited<ReturnType<typeof bridgeApprovalMachine>> | null = null;
  let snap = $state<BridgeApprovalSnapshot | null>(null);
  let error = $state('');
  // Which approved card's rotate-confirm is open (pubkey_hex), or null. Inline
  // reveal — matches the linux modal/other-app sheet at the same IDs (ui.yaml
  // admin-bridges-rotate-confirm notes: "route on web"; precedent:
  // mail-rotate-keys-confirm in MailSettingsSection.svelte renders inline too).
  let rotatingPubkeyHex = $state<string | null>(null);

  const busy = $derived(snap?.status === 'Working');

  function applySnapshot(): void {
    if (!machine) return;
    snap = machine.snapshot() as unknown as BridgeApprovalSnapshot;
    if (snap?.error) error = snap.error;
  }

  // Re-reads the page on mount AND on every later re-entry (`afterNavigate`
  // below) — there is no client-side admin cache (admin.md § Persistence:
  // "every page re-reads on entry/refresh"), so a bridge enrolled or approved
  // out of band while the admin sits elsewhere must show up on return.
  //
  // ⚠ A SvelteKit `goto()` to the route this page is ALREADY mounted on does
  // not remount it, so `onMount` alone only ever fires once per visit — every
  // other app's own "navigate to this page" always re-fetches (linux's
  // `update_pending_bridges` docs "rebuilt on every load"; tui/android mirror
  // it). `afterNavigate` is the reliable per-navigation-EVENT signal (fires
  // even when the destination route is unchanged), mirroring
  // `WebSettingsSection.svelte`'s identical same-route-revisit fix and
  // linux's `set_visible_child_forced`.
  //
  // Builds `machine` here too (not just in a separate onMount branch): if the
  // page first mounted before the identity store had a secret (a session
  // injected after initial load), `machine` never got created, and this is
  // also the path a later re-entry uses to pick that up.
  async function hydrate(): Promise<void> {
    const id = $identity;
    if (!id?.secretHex) return;
    try {
      await ensureWasm();
      if (!machine) machine = await bridgeApprovalMachine(id.secretHex);
      await machine.hydrate();
      applySnapshot();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  onMount(() => {
    void hydrate();
  });
  afterNavigate(() => {
    void hydrate();
  });

  async function dispatch(action: unknown): Promise<void> {
    if (!machine) return;
    error = '';
    try {
      await machine.dispatch(action);
      applySnapshot();
    } catch (e) {
      applySnapshot();
      if (!error) error = e instanceof Error ? e.message : String(e);
    }
  }

  function firstSeenText(v: PendingBridgeView): string {
    return v.first_seen_at ? new Date(v.first_seen_at).toLocaleString() : '—';
  }

  function approvedAtText(v: ApprovedBridgeView): string {
    return v.approved_at ? new Date(v.approved_at).toLocaleString() : '—';
  }

  function openRotateConfirm(pubkeyHex: string): void {
    rotatingPubkeyHex = pubkeyHex;
  }

  function closeRotateConfirm(): void {
    rotatingPubkeyHex = null;
  }

  async function confirmRotate(pubkeyHex: string): Promise<void> {
    await dispatch({ Rotate: { pubkey_hex: pubkeyHex } });
    rotatingPubkeyHex = null;
  }

  // Friendly per-role display name (admin.md § Bridge display naming; canonical
  // role→name table in bridges.md § Active bridges). One MDA bridge serves both
  // mail and calendar, so only the MDA card names calendar. The map lives in
  // shared Rust (`fauna_client_mail_settings::bridge_display_name`, WASM twin
  // `bridgeDisplayName`) — the same source linux/windows/android/apple consume —
  // so the role→name policy is single-sourced (priority #2).
  function roleName(role: string): string {
    return resolveLocalized(bridgeDisplayName(role));
  }
</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.admin.bridges_pending.title}</h1>

<MessageBanner bind:error />

<p class="muted small">{t.admin.bridges_pending.description}</p>

<h2 class="section-heading">{t.admin.bridges_pending.pending_section}</h2>
<div class="cards">
  {#if (snap?.pending.length ?? 0) === 0}
    <p class="muted small">{t.admin.bridges_pending.empty}</p>
  {/if}
  {#each snap?.pending ?? [] as v (v.pubkey_hex)}
    <div class="card" data-testid={IDS.ADMIN_BRIDGES_PENDING_CARD}>
      <div class="card-name" data-testid={IDS.ADMIN_BRIDGES_PENDING_CARD_NAME}>
        {roleName(v.requested_role)}
      </div>
      <div class="field">
        <span class="label">{t.admin.bridges_pending.role}</span>
        <span class="value" data-testid={IDS.ADMIN_BRIDGES_PENDING_REQUESTED_ROLE}>{v.requested_role}</span>
      </div>
      <div class="field">
        <span class="label">{t.admin.bridges_pending.pubkey}</span>
        <span class="value mono" data-testid={IDS.ADMIN_BRIDGES_PENDING_PUBKEY_HEX}>{v.pubkey_hex}</span>
      </div>
      <div class="field">
        <span class="label">{t.admin.bridges_pending.source_ip}</span>
        <span class="value" data-testid={IDS.ADMIN_BRIDGES_PENDING_SOURCE_IP}
          >{v.source_ip ?? t.admin.bridges_pending.source_ip_unknown}</span>
      </div>
      <div class="field">
        <span class="label">{t.admin.bridges_pending.first_seen}</span>
        <span class="value" data-testid={IDS.ADMIN_BRIDGES_PENDING_FIRST_SEEN_AT}>{firstSeenText(v)}</span>
      </div>
      <div class="actions">
        <button
          class="btn small danger"
          data-testid={IDS.ADMIN_BRIDGES_PENDING_REJECT_BUTTON}
          disabled={busy}
          onclick={() => dispatch({ Reject: { pubkey_hex: v.pubkey_hex } })}
        >
          {t.admin.bridges_pending.reject}
        </button>
        <button
          class="btn small primary"
          data-testid={IDS.ADMIN_BRIDGES_PENDING_APPROVE_BUTTON}
          disabled={busy}
          onclick={() => dispatch({ Approve: { pubkey_hex: v.pubkey_hex, role: v.requested_role } })}
        >
          {t.admin.bridges_pending.approve}
        </button>
      </div>
    </div>
  {/each}
</div>

<h2 class="section-heading">{t.admin.bridges_pending.approved_section}</h2>
<div class="cards">
  {#if (snap?.approved.length ?? 0) === 0}
    <p class="muted small">{t.admin.bridges_pending.approved_empty}</p>
  {/if}
  {#each snap?.approved ?? [] as v (v.pubkey_hex)}
    <div class="card" data-testid={IDS.ADMIN_BRIDGES_APPROVED_CARD}>
      <div class="card-name" data-testid={IDS.ADMIN_BRIDGES_APPROVED_CARD_NAME}>
        {roleName(v.role)}
      </div>
      <div class="field">
        <span class="label">{t.admin.bridges_pending.role}</span>
        <span class="value" data-testid={IDS.ADMIN_BRIDGES_APPROVED_ROLE}>{v.role}</span>
      </div>
      <div class="field">
        <span class="label">{t.admin.bridges_pending.pubkey}</span>
        <span class="value mono" data-testid={IDS.ADMIN_BRIDGES_APPROVED_PUBKEY_HEX}>{v.pubkey_hex}</span>
      </div>
      <div class="field">
        <span class="label">{t.admin.bridges_pending.approved_at}</span>
        <span class="value" data-testid={IDS.ADMIN_BRIDGES_APPROVED_APPROVED_AT}>{approvedAtText(v)}</span>
      </div>
      <div class="actions">
        <button
          class="btn small danger"
          data-testid={IDS.ADMIN_BRIDGES_APPROVED_ROTATE_BUTTON}
          disabled={busy}
          onclick={() => openRotateConfirm(v.pubkey_hex)}
        >
          {t.admin.bridges_pending.rotate}
        </button>
      </div>
      {#if rotatingPubkeyHex === v.pubkey_hex}
        <div class="confirm-box">
          <strong>{t.admin.bridges_rotate.title}</strong>
          <p class="muted small" data-testid={IDS.ADMIN_BRIDGES_ROTATE_WARNING_TEXT}>{t.admin.bridges_rotate.warning}</p>
          <div class="actions">
            <button
              class="btn small"
              data-testid={IDS.ADMIN_BRIDGES_ROTATE_CANCEL_BUTTON}
              disabled={busy}
              onclick={closeRotateConfirm}
            >
              {t.admin.bridges_rotate.cancel}
            </button>
            <button
              class="btn small danger"
              data-testid={IDS.ADMIN_BRIDGES_ROTATE_CONFIRM_BUTTON}
              disabled={busy}
              onclick={() => confirmRotate(v.pubkey_hex)}
            >
              {t.admin.bridges_rotate.confirm}
            </button>
          </div>
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  h1 { margin-bottom: 1rem; font-size: 1.5rem; }
  .section-heading { font-size: 1rem; margin: 1.25rem 0 0.25rem; color: var(--text, #e6edf3); }
  .muted { color: var(--text-muted, #8b949e); }
  .small { font-size: 0.8rem; }
  .cards { display: flex; flex-direction: column; gap: 0.75rem; margin-top: 1rem; }
  .confirm-box {
    margin-top: 0.75rem;
    padding: 0.75rem;
    border: 1px solid var(--danger, #f85149);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 0.375rem;
  }
  .card {
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    padding: 0.75rem 1rem;
    background: var(--bg-surface, #161b22);
  }
  .card-name {
    font-weight: 600;
    font-size: 1rem;
    margin-bottom: 0.5rem;
    color: var(--text, #e6edf3);
  }
  .field { display: flex; gap: 0.5rem; padding: 0.125rem 0; font-size: 0.875rem; }
  .label {
    min-width: 90px;
    color: var(--text-muted, #8b949e);
    text-transform: uppercase;
    font-size: 0.7rem;
    letter-spacing: 0.05em;
    align-self: center;
  }
  .value { word-break: break-all; }
  .mono { font-family: monospace; font-size: 0.8rem; }
  .actions { display: flex; gap: 0.5rem; justify-content: flex-end; margin-top: 0.5rem; }
  .btn {
    padding: 0.375rem 0.75rem;
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    background: var(--bg-surface, #161b22);
    color: var(--text, #e6edf3);
    cursor: pointer;
    font-size: 0.8rem;
  }
  .btn:hover { background: var(--bg-hover, #1c2128); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { border-color: var(--accent, #58a6ff); color: var(--accent, #58a6ff); }
  .btn.danger { border-color: var(--danger, #f85149); color: var(--danger, #f85149); }
  .btn.small { font-size: 0.8rem; padding: 0.25rem 0.625rem; }
</style>
