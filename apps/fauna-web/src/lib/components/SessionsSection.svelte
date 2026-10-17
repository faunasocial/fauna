<script lang="ts">
  // Settings → Sessions (`docs/goal/ui/sessions.md`; ui.yaml page `sessions`,
  // reached `{"view":"settings","id":"sessions"}`) — where this account is
  // signed in right now, as one escalation ladder: the list (revoke one), sign
  // out everywhere else, the 24-hour lock. The rail slot is directly after
  // Devices (`settings.md` § Navigation model). tui leads
  // (`apps/fauna-tui/src/settings/sessions.rs`); linux is the same page region
  // for region (`apps/fauna-linux/src/settings/sessions.rs`).
  //
  // **A paint shell over shared Rust.** The rows, their order, kinds and marks
  // are `fauna_client_account::sessions_view`'s fold; the act-then-refold pass
  // (a failed re-read keeps the painted rows) and the lock's action-arm word
  // gate run in the wasm twin (`libs/fauna-wasm/src/sessions.rs`). What stays
  // here is what § Where logic lives leaves to app glue: the render, the two
  // confirm states, and the time formatting — web formats epoch seconds itself,
  // `format_unix_local` being native-only. No page machine: the page re-reads
  // on every visit and re-snapshots after each act.
  //
  // **A landed lock** ends every session this account holds, this tab's too:
  // the acts stand disabled and the tab leaves the shell for the launch flow,
  // whose challenge meets the lock and paints the locked surface (leg 2,
  // `escalateAccountLocked`).
  import { onMount, onDestroy } from 'svelte';
  import { identity } from '$lib/store';
  import { ensureWasm, accountEnrolledDeviceRow, sessionsLockoutConfirmWord } from '$lib/wasm';
  import type { SessionRow, SessionsInput, SessionsPage, SessionsSnapshot } from '$lib/wasm';
  import { sessionsLoad, sessionsRevoke, sessionsRevokeOthers, sessionsLockout } from '$lib/rpc';
  import { ownTokenIds, currentTokenId } from '$lib/api';
  import { sessionDevicesMachine } from '$lib/devices-session';
  import type { DevicesSnapshot } from '$lib/devices-machine';
  import { getDeviceId } from '$lib/device-id';
  import { localTimestamp } from '$lib/value-format';
  import { escalateAccountLocked } from '$lib/post-auth-escalation';
  import { setPaintedSessionTokenIds } from '$lib/sessions-painted';
  import { resolveLocalized } from '$lib/i18n/localized';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // The Settings shell's shared `error-message` banner, as every sub-page.
  let { error = $bindable('') } = $props();

  let snapshot = $state<SessionsSnapshot | null>(null);
  /** A refusal this page raised itself (a transport failure the twin never
   *  saw) — painted ahead of the snapshot's error, cleared by the next pass. */
  let localError = $state('');
  let revokeOthersArmed = $state(false);
  let lockWord = $state('');
  let busy = $state(false);
  /** The lock landed: every session is gone, this tab's too. */
  let locked = $state(false);
  let confirmWord = $state('LOCK');
  let unsubDevices: (() => void) | null = null;
  let roster: DevicesSnapshot['devices'] = [];
  let enrolledRow: string | null = null;

  const idle = $derived(!busy && !locked);

  /** `fauna_core::format::format_unix_local`'s shape, through the shared
   *  `localTimestamp` (`$lib/value-format`). */
  function when(secs: number): string {
    return localTimestamp(secs * 1000);
  }

  function detail(row: SessionRow): string {
    return t.sessions.detail({
      created: when(row.createdAt),
      last_used: when(row.lastUsedAt),
      expires: when(row.expiresAt),
      address: row.ipAddress ?? t.sessions.address_not_recorded,
    });
  }

  /** This browser's roster row — the Devices page's This-device read door
   *  (`devices.md` § This-device marker): the enrolled row once latched, else
   *  the id this browser's enrollment targets. */
  function thisDeviceRow(): string | null {
    if (enrolledRow) return enrolledRow;
    const actorId = $identity?.actorId;
    if (!actorId) return null;
    try {
      return getDeviceId(actorId);
    } catch {
      return null;
    }
  }

  /** Built afresh for every call: the own ids are read at call time, never off
   *  a painted list (`devices.md` § The client's own session). */
  function input(secretHex: string): SessionsInput {
    return {
      ownTokenIds: ownTokenIds(secretHex),
      currentTokenId: currentTokenId(secretHex),
      devices: roster.map((d) => ({
        deviceId: d.device_id,
        principal: d.principal ?? null,
        label: d.label,
      })),
      thisDeviceRow: thisDeviceRow(),
      previous: snapshot,
    };
  }

  function paintError(): void {
    const folded = snapshot?.error ? resolveLocalized(snapshot.error) : '';
    error = localError || folded || '';
  }

  function settle(page: SessionsPage): void {
    snapshot = page.snapshot;
    localError = '';
    setPaintedSessionTokenIds(page.snapshot.rows.map((r) => r.tokenId));
    paintError();
    if (page.locked) {
      locked = true;
      escalateAccountLocked(null);
    }
  }

  /** One pass: act (or not), re-read, re-fold. A rejection is a transport
   *  failure the twin never answered — painted, with the rows kept. */
  async function pass(
    run: (secretHex: string, input: SessionsInput) => Promise<SessionsPage>,
    failed: (error: string) => string,
  ): Promise<void> {
    const id = $identity;
    if (!id || locked) return;
    busy = true;
    try {
      settle(await run(id.secretHex, input(id.secretHex)));
    } catch (e: unknown) {
      localError = failed(e instanceof Error ? e.message : String(e));
      paintError();
    } finally {
      busy = false;
    }
  }

  function revoke(tokenId: string): void {
    revokeOthersArmed = false;
    void pass(
      (s, i) => sessionsRevoke(s, i, tokenId),
      (e) => t.sessions.act_failed({ error: e }),
    );
  }

  function revokeOthers(): void {
    revokeOthersArmed = false;
    void pass(sessionsRevokeOthers, (e) => t.sessions.act_failed({ error: e }));
  }

  function lock(): void {
    // The word is checked again in the action arm (the twin) — a wrong word is
    // refused there out loud and reaches no nest.
    const word = lockWord;
    void pass(
      (s, i) => sessionsLockout(s, i, word),
      (e) => t.sessions.act_failed({ error: e }),
    );
  }

  onMount(async () => {
    const id = $identity;
    if (!id) return;
    try {
      await ensureWasm();
      confirmWord = sessionsLockoutConfirmWord();
      // The roster names device-minted sessions — rendered by the session's
      // one shared Devices machine, which owns the sealed-label render.
      const session = await sessionDevicesMachine(id.secretHex, () => {});
      unsubDevices = session.unlisten;
      await session.machine.refresh();
      const raw = session.machine.snapshotJson();
      roster = raw ? (JSON.parse(raw) as DevicesSnapshot).devices : [];
      enrolledRow = await accountEnrolledDeviceRow();
    } catch (e: unknown) {
      // No roster is not fatal: device sign-ins then read as the neutral
      // third kind, never as an error — the list itself is what matters.
      console.warn('[sessions] roster unavailable:', e);
    }
    await pass(sessionsLoad, (e) => t.sessions.load_failed({ error: e }));
  });

  onDestroy(() => {
    unsubDevices?.();
    setPaintedSessionTokenIds(null);
  });
</script>

<section class="card">
  {#if snapshot}
    {#if snapshot.rows.length === 0}
      <p data-testid={IDS.SESSIONS_EMPTY} class="muted">{t.sessions.empty}</p>
    {/if}
    {#each snapshot.rows as row (row.tokenId)}
      <!-- Indexed by position (`session-card[i]`), the bridge's indexing — the
           order is the shared fold's, and this tab's own row is card 0. -->
      <div data-testid={IDS.SESSION_CARD} class="session-card">
        <div class="session-head">
          <span data-testid={IDS.SESSION_KIND} class="kind">{resolveLocalized(row.kind)}</span>
          {#if row.mark}
            <span data-testid={IDS.SESSION_THIS_MARK_BADGE} class="badge">{resolveLocalized(row.mark)}</span>
          {/if}
        </div>
        <p data-testid={IDS.SESSION_DETAIL} class="muted small">{detail(row)}</p>
        {#if row.canRevoke}
          <button
            data-testid={IDS.SESSION_REVOKE_BUTTON}
            class="btn"
            disabled={!idle}
            onclick={() => revoke(row.tokenId)}
          >{t.sessions.revoke}</button>
        {/if}
      </div>
    {/each}
  {:else}
    <p class="muted">{t.common.loading}</p>
  {/if}
</section>

<section class="card">
  <!-- Sign out everywhere else: the two-press inline confirm (`ui/sessions.md`
       § The ruling, item 3 — no native-dialog variant). -->
  {#if revokeOthersArmed}
    <div class="button-group">
      <button
        data-testid={IDS.SESSIONS_REVOKE_OTHERS_CONFIRM_BUTTON}
        class="btn danger"
        disabled={!idle}
        onclick={revokeOthers}
      >{t.sessions.revoke_others_confirm}</button>
      <button
        data-testid={IDS.SESSIONS_REVOKE_OTHERS_CANCEL_BUTTON}
        class="btn"
        onclick={() => (revokeOthersArmed = false)}
      >{t.sessions.revoke_others_cancel}</button>
    </div>
  {:else}
    <button
      data-testid={IDS.SESSIONS_REVOKE_OTHERS_BUTTON}
      class="btn"
      disabled={!idle}
      onclick={() => (revokeOthersArmed = true)}
    >{t.sessions.revoke_others}</button>
  {/if}
  <!-- REQUIRED copy: the honest bound of what revoking does. -->
  <p data-testid={IDS.SESSIONS_REVOKE_NOTE} class="muted small">{t.sessions.revoke_note}</p>
</section>

<section class="card">
  <!-- The lock: the warning is always visible — never behind the confirm. -->
  <p data-testid={IDS.SESSIONS_LOCKOUT_WARNING} class="warning">{t.sessions.lockout_warning}</p>
  <input
    data-testid={IDS.SESSIONS_LOCKOUT_CONFIRM_FIELD}
    type="text"
    class="input"
    autocomplete="off"
    placeholder={t.sessions.lockout_confirm_placeholder}
    bind:value={lockWord}
  />
  <button
    data-testid={IDS.SESSIONS_LOCKOUT_BUTTON}
    class="btn danger"
    disabled={!idle || lockWord !== confirmWord}
    onclick={lock}
  >{t.sessions.lockout_button}</button>
</section>

<style>
  .session-card {
    padding: 0.75rem 0;
    border-bottom: 1px solid var(--border);
  }
  .session-card:last-child {
    border-bottom: none;
  }
  .session-head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .kind {
    font-weight: 600;
  }
  .badge {
    font-size: 0.75rem;
    padding: 0.125rem 0.5rem;
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--text-muted);
  }
  .small {
    font-size: 0.8125rem;
  }
  .warning {
    color: var(--text);
  }
  .button-group {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
</style>
