<script lang="ts">
  // Settings → Devices (sync-file-set-ui-unification, 2026-06-28): the device
  // ROSTER only, rendered inside the Settings shell. Formerly half of the
  // top-level Devices/Peers page; the folder wizard / list / conflicts moved to
  // Settings → Folders (FoldersSection.svelte). A dumb renderer of the shared
  // `DevicesMachine` (`libs/fauna-devices-machine`, WASM twin
  // `libs/fauna-wasm-folders`): build the machine over the WS-RPC connection →
  // refresh() → render the roster slice → forward device-remove back through the
  // machine. No roster logic in the SPA (priority #2). Behaviour + IDs:
  // docs/goal/ui/devices.md, ui.yaml § devices.
  import { identity } from '$lib/store';
  import {
    ensureWasm,
    deviceStatusLabel,
    devicePlaceLabel,
    hexFull,
    shortId,
    accountEnrollmentNotice,
    accountEnrolledDeviceRow,
  } from '$lib/wasm';
  import { sessionDevicesMachine } from '$lib/devices-session';
  import {
    custodyDegradedBadgeKey,
    custodyFacetLoad,
    custodyRevoke,
  } from '$lib/wasm-folders';
  import { custodyHeldBytesText, custodyReceiptStatusText } from '$lib/custody';
  import { getDeviceId } from '$lib/device-id';
  import { resolveLocalized, resolveLocalizedNested } from '$lib/i18n/localized';
  import { onMount, onDestroy } from 'svelte';
  import { onStoreChange } from '$lib/store-change';
  import { t } from '$lib/i18n/strings';
  import { type CustodyHolderRowView, type DevicesSnapshot } from '$lib/devices-machine';
  import type { DevicesMachine } from '../../../static/fauna_wasm_folders.js';
  import { IDS } from '$lib/generated/uiIds';

  // The page-level error surface is the Settings shell's shared MessageBanner
  // (`error-message`); we write into its bound `error` so a roster read/remove
  // failure renders there, matching every other settings sub-page.
  let { error = $bindable('') } = $props();

  let machine: DevicesMachine | null = null;
  let snap = $state<DevicesSnapshot | null>(null);
  let ready = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;
  let unlisten: (() => void) | null = null;
  // This browser's device id for the signed-in account — drives
  // `device-this-mark-badge` (`devices.md` § This-device marker). Per account,
  // so it follows the session identity; stable for a given account. A failed
  // read leaves no row marked rather than breaking the page.
  // The row this browser's enrollment registered on, read off the account
  // runtime (`ui/devices.md` § This-device marker — the same read door every
  // hosting app uses). `null` before the enrollment has latched, when the
  // marker falls back to the derived id the enrollment targets.
  let enrolledRow = $state<string | null>(null);
  const localDeviceId = $derived.by(() => {
    if (enrolledRow) return enrolledRow;
    const actorId = $identity?.actorId;
    if (!actorId) return null;
    try {
      return getDeviceId(actorId);
    } catch (e) {
      console.warn('device id unavailable:', e);
      return null;
    }
  });

  // ── T16 custody facet, owner side (devices.md § Custody facet, piece 2) ──
  //
  // NOT `DevicesSnapshot` state: the facet folds the `fauna.state.custody-ceremony` entries the
  // deliberately-keyless machine cannot read, so it rides its own load on the
  // page's own edge — the web twin of linux's `connect_map` hydrate and
  // android's `DevicesVM.loadCustodyFacet`.
  //
  // Piece 2 ONLY. Pieces 1 (keyless-posture badge) and 3 (the held-for-others
  // card with its budget input and stop control) both need the W3 (account-data-plane.md § Workstreams) account
  // store, which has no wasm twin — and `devices.md` defines piece 3's card AS
  // those two controls, so rendering it with them disabled would contradict the
  // ratified text and offer controls that cannot succeed.
  let custodyRows = $state<CustodyHolderRowView[]>([]);
  // A custody whose accept bound the host's NEST belongs to the Nests page's
  // `nest-trust-custody-*` family — one custody never renders in both places
  // (the nest-custodian identity fact, ruled 2026-08-17).
  const custodyDeviceRows = $derived(custodyRows.filter((r) => r.custodian_nest_url === null));

  async function loadCustody(): Promise<void> {
    const id = $identity;
    if (!machine || !id) return;
    // A null fold is a transient (unreadable config this pass) — keep the rows
    // already painted rather than telling the owner their custodians are gone.
    const facet = await custodyFacetLoad(machine, id.secretHex);
    if (facet) custodyRows = facet.rows;
  }

  // Byte ids cross serde as plain number arrays, never `Uint8Array` — the same
  // normalization `task-delegation.ts` already does for a nest actor pubkey.
  function hex(bytes: number[]): string {
    return hexFull(Uint8Array.from(bytes));
  }

  // Web is the ONE app that formats the receipt timestamp itself: the shared
  // `format_unix_local` needs the OS timezone database, which wasm lacks, so
  // the boundary deliberately hands over epoch seconds. Matches `formatTime`
  // above rather than inventing a second date style for this page.
  function formatWhen(secs: number): string {
    return new Date(secs * 1000).toLocaleString();
  }

  async function revokeCustody(row: CustodyHolderRowView): Promise<void> {
    const id = $identity;
    if (!machine || !id) return;
    // Carries the row's grant id + accept-bound custodian key, never an index:
    // a refold re-orders rows. The act's own nest-before-record ordering lives
    // in shared Rust — this only routes the outcome.
    const err = await custodyRevoke(machine, id.secretHex, row.grant_id, row.custodian_key);
    // Never a silent drop (e2e convention 11): the act's error goes to the
    // page's `error-message` banner, and either way we refold so a successful
    // revoke drops its row without waiting for the next visit.
    error = err ?? '';
    await loadCustody();
  }

  // The standing refusal of this browser's enrollment — the tier device cap
  // (`ui/devices.md` § State & data shape): the one rendered sentence, read
  // off the account runtime on every hydrate. Not a gesture error: it stays up
  // until the slot no longer records it, and a roster gesture's own error wins
  // while that stands.
  let enrollmentNotice = $state<string | null>(null);

  function paintError(): void {
    const gesture = snap?.error ? resolveLocalized(snap.error) : '';
    error = gesture || enrollmentNotice || '';
  }

  // The two account-runtime reads, on the page's own edge (mount, and the
  // periodic re-read). Either answers `null` while no runtime runs.
  async function loadAccountFacts(): Promise<void> {
    [enrollmentNotice, enrolledRow] = await Promise.all([
      accountEnrollmentNotice(),
      accountEnrolledDeviceRow(),
    ]);
    paintError();
  }

  function applySnapshot(): void {
    if (!machine) return;
    const raw = machine.snapshotJson();
    snap = raw ? (JSON.parse(raw) as DevicesSnapshot) : null;
    paintError();
  }

  onMount(async () => {
    const id = $identity;
    if (!id) {
      ready = true;
      return;
    }
    try {
      await ensureWasm();
      // The session's ONE machine, shared with the Folders section and wired
      // once — label custody included (`devices-session.ts`).
      const session = await sessionDevicesMachine(id.secretHex, () => applySnapshot());
      machine = session.machine;
      unlisten = session.unlisten;
      // Paint the last-known rows now; the refresh below brings them current.
      applySnapshot();
      ready = true;
      await machine.refresh();
      applySnapshot();
      // The custody facet's own load, on the same page edge (devices.md §
      // Custody facet). Deliberately NOT awaited into the roster's critical
      // path: a slow config read must not hold the device list back, and a
      // failure here degrades to "no custodians shown" rather than an empty
      // page — the roster is what this page is for.
      void loadCustody();
      void loadAccountFacts();
      // Periodic re-read so a device registered out-of-band shows up — and
      // the enrollment's standing refusal comes down once a pass clears it.
      timer = setInterval(() => {
        machine?.refresh();
        void loadAccountFacts();
      }, 15000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
      ready = true;
    }
  });

  // The store-change notice (`$lib/store-change`): the keyed set, the enrolled
  // row and the custody records rest in the account store, so the open page
  // runs the periodic re-read's own body at once, and re-folds the custody
  // facet — a fold only (a null fold keeps the rows), never a ceremony drive.
  const unsubStoreChange = onStoreChange(() => {
    machine?.refresh();
    void loadAccountFacts();
    void loadCustody().catch(() => {});
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
    unsubStoreChange();
    unlisten?.();
  });

  async function removeDevice(index: number): Promise<void> {
    await machine?.removeDevice(index);
    applySnapshot();
  }

  // `device-p2p-participation-toggle[index]`: the machine takes the arm (on
  // web always the request-off one) and refreshes, or paints the refusal on
  // `error-message`; the re-snapshot repaints the row either way.
  async function setP2pParticipation(index: number, on: boolean): Promise<void> {
    await machine?.setP2pParticipation(index, on);
    applySnapshot();
  }

  // ── Signed-in devices without a matching entry (devices.md § Members without
  // a matching entry) ──
  //
  // The two-step remove's arm state is the armed card's KEY (its `device_id`),
  // never a position: the key travels from the arm to the confirm, so a refresh
  // that reshapes the list between the two cannot retarget the confirm. A fresh
  // mount starts disarmed (component state), and an armed key the list no
  // longer carries disarms.
  const members = $derived(snap?.members ?? []);
  let armedMember = $state<string | null>(null);
  const armedKey = $derived(
    armedMember !== null && members.some((m) => m.device_id === armedMember) ? armedMember : null,
  );

  async function confirmRemoveMember(deviceId: string): Promise<void> {
    armedMember = null;
    // A refusal or failed write lands on the machine's `error`, which
    // `applySnapshot` paints on `error-message` — the row gesture's path.
    await machine?.removeMember(deviceId);
    applySnapshot();
  }

  function formatTime(epoch: number | null): string {
    if (epoch == null || epoch === 0) return t.common.never;
    return new Date(epoch * 1000).toLocaleString();
  }

  // Single, page-level (never per-`device-card`) copy of THIS client's own
  // actor ID, for handing to a new device being paired (`devices.md` § Layout
  // & flow point 2; ui.yaml `peer-actor-id-copy-btn` is `indexed: false`).
  // Mirrors the Settings → Account identity row's `copyActorId`
  // (`routes/settings/[[subpage]]/+page.svelte`).
  let actorIdCopied = $state(false);
  async function copyActorId(): Promise<void> {
    if ($identity?.actorId) {
      await navigator.clipboard.writeText($identity.actorId);
      actorIdCopied = true;
      setTimeout(() => { actorIdCopied = false; }, 2000);
    }
  }
</script>

{#if !ready}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.identity_required}</p>
{:else}
  <section class="section">
    <h2>{t.devices.my_devices}</h2>
    <!-- Single page-level instance (never per-device-card), rendered
         unconditionally — pairing the FIRST device is exactly when copying
         this client's own actor ID is needed. -->
    <p class="identity-copy-row">
      <span class="mono">{$identity.actorId}</span>
      <button
        class="btn btn-small"
        data-testid={IDS.PEER_ACTOR_ID_COPY_BTN}
        onclick={copyActorId}
      >{actorIdCopied ? t.common.copied : t.devices.copy_actor_id}</button>
    </p>
    {#if (snap?.devices.length ?? 0) === 0}
      <p class="muted">{t.devices.no_devices}</p>
    {:else}
      <div class="device-grid">
        {#each snap?.devices ?? [] as device, i}
          <div class="device-card" data-testid={IDS.DEVICE_CARD}>
            <div class="device-header">
              <span class="status-dot" class:online={device.online}></span>
              <strong data-testid={IDS.DEVICE_NAME}>{device.label || shortId(device.device_id)}</strong>
              <!-- Slice F guardian-enrolled-device marker (family-safety.md § Full
                   visibility): the ward's own list renders it (transparency by
                   construction); absent on an unsupervised account. -->
              {#if device.guardian_marked}
                <span class="badge guardian-badge" data-testid={IDS.DEVICE_GUARDIAN_MARK_BADGE}>{t.devices.guardian_marked_badge}</span>
              {/if}
              <!-- Not mutually exclusive with the guardian badge above — a
                   guardian marking their own enrolled device can legitimately
                   carry both (`devices.md` § This-device marker). -->
              {#if device.device_id === localDeviceId}
                <span class="badge this-device-badge" data-testid={IDS.DEVICE_THIS_MARK_BADGE}>{t.devices.this_device_badge}</span>
                <!-- This browser's own key fingerprint — the user's half of the
                     comparison the member group below asks for; the SAME shared
                     formatter as every member card, absent until the account
                     runtime has answered (`devices.md` § Members without a
                     matching entry, the elimination bullet). -->
                {#if snap?.own_fingerprint}
                  <span class="own-fingerprint mono" data-testid={IDS.DEVICE_OWN_FINGERPRINT}>{t.devices.own_fingerprint({ fingerprint: snap.own_fingerprint })}</span>
                {/if}
              {/if}
            </div>
            <p class="device-meta">
              <span data-testid={IDS.DEVICE_STATUS}>{resolveLocalized(deviceStatusLabel(device.online))}</span>
              {#if device.last_seen_at}
                &middot; {t.devices.last_seen} {formatTime(device.last_seen_at)}
              {/if}
            </p>
            <!-- One `device-folder-role-badge` chip per folder this device
                 carries, naming its place (three flags) in that set — the
                 shared composed label (`fauna_core::format::device_place_label`)
                 built from the wizard's `devices.wizard.place_*` words, resolved
                 NESTED because its template arguments are themselves keys.
                 Indexed: a device in three sets paints three chips; nothing
                 renders when the device carries no sets (`devices.md` §
                 Element table — `device-folder-role-badge`). -->
            {#if device.folders.length > 0}
              <div class="device-roles">
                {#each device.folders as fs}
                  <span class="badge" data-testid={IDS.DEVICE_FOLDER_ROLE_BADGE}>{resolveLocalizedNested(devicePlaceLabel(fs.originates, fs.accepts, fs.applies_deletes))}</span>
                {/each}
              </div>
            {/if}
            <!-- `device-p2p-participation-toggle` (`p2p.md` § Per-device
                 participation — rule 5's off switch; ID user-approved
                 2026-09-25), drawn exactly as the machine painted the row.
                 This tab runs no listener, so it has no own row: every row
                 paints the request-off arm (`own: false`), actionable only
                 while that device may still be on with no request pending.
                 A null paint is a row not painted yet — nothing to draw. -->
            {#if device.p2p_participation_paint}
              {@const paint = device.p2p_participation_paint}
              <label
                class="p2p-participation"
                data-testid={IDS.DEVICE_P2P_PARTICIPATION_TOGGLE}
                data-checked={paint.checked ? 'true' : 'false'}
                aria-disabled={paint.actionable ? 'false' : 'true'}
              >
                <input
                  type="checkbox"
                  checked={paint.checked}
                  disabled={!paint.actionable}
                  onchange={() => setP2pParticipation(i, !paint.checked)}
                />
                {resolveLocalized(paint.label)}
              </label>
            {/if}
            <div class="device-actions">
              <button class="btn btn-small btn-danger" data-testid={IDS.DEVICE_REMOVE_BUTTON} onclick={() => removeDevice(i)}>{t.common.remove}</button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <!-- Signed-in devices without a matching entry — below the roster, never
       intermixed with it, and nothing at all (no title, no note) when the fleet
       lists nobody (`devices.md` § Members without a matching entry). A member
       has no name: the card shows its key fingerprint and the sign-in time the
       device itself claims. The title has no ui.yaml id of its own. -->
  {#if members.length > 0}
    <section class="section">
      <h2>{t.devices.members_title}</h2>
      <p class="member-note" data-testid={IDS.DEVICE_MEMBER_NOTE}>{t.devices.member_note}</p>
      <div class="device-grid">
        {#each members as member (member.device_id)}
          <div class="device-card" data-testid={IDS.DEVICE_MEMBER_CARD}>
            <div class="device-header">
              <strong class="mono" data-testid={IDS.DEVICE_MEMBER_FINGERPRINT}>{t.devices.member_fingerprint({ fingerprint: member.fingerprint })}</strong>
            </div>
            <!-- The snapshot carries unix ms; the section's absolute-timestamp
                 door (`formatWhen`, the custody receipt's) takes seconds. -->
            <p class="device-meta" data-testid={IDS.DEVICE_MEMBER_ENROLLED_AT}>
              {t.devices.member_enrolled_at({ when: formatWhen(member.enrolled_at_ms / 1000) })}
            </p>
            <div class="device-actions">
              {#if armedKey === member.device_id}
                <button
                  class="btn btn-small btn-danger"
                  data-testid={IDS.DEVICE_MEMBER_REMOVE_CONFIRM_BUTTON}
                  onclick={() => confirmRemoveMember(member.device_id)}
                >{t.devices.member_remove_confirm}</button>
                <button
                  class="btn btn-small"
                  data-testid={IDS.DEVICE_MEMBER_REMOVE_CANCEL_BUTTON}
                  onclick={() => { armedMember = null; }}
                >{t.common.cancel}</button>
              {:else}
                <button
                  class="btn btn-small btn-danger"
                  data-testid={IDS.DEVICE_MEMBER_REMOVE_BUTTON}
                  onclick={() => { armedMember = member.device_id; }}
                >{t.common.remove}</button>
              {/if}
            </div>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  <!-- The T16 custody facet's owner side — "who holds my data". Hidden
       entirely when the account has no custodians: a titled-but-empty section
       reads as a feature that failed to load. Same rule as linux's
       `build_custody_section` and android's `CustodyHolderCard`. -->
  {#if custodyDeviceRows.length > 0}
    <section class="section">
      <h2>{t.devices.custody_holder_section}</h2>
      <div class="device-grid">
        {#each custodyDeviceRows as row}
          <div class="device-card" data-testid={IDS.CUSTODY_HOLDER_CARD}>
            <div class="device-header">
              <!-- The counterpart account through the SAME shared `short_id`
                   every app uses, so an actor reads identically across the
                   seven UIs. -->
              <strong data-testid={IDS.CUSTODY_HOLDER_NAME}>{shortId(hex(row.host))}</strong>
            </div>
            <!-- Three states, three strings — never collapsed, never empty (the
                 A7 honesty rule): a stale custodian must read as degraded
                 redundancy the owner can see, not as an absent row. -->
            <p class="device-meta" class:custody-stale={row.receipt_state === 'Stale'} data-testid={IDS.CUSTODY_HOLDER_RECEIPT_STATUS}>
              {custodyReceiptStatusText(row.receipt, formatWhen)}
            </p>
            <p class="device-meta" data-testid={IDS.CUSTODY_HOLDER_HELD_BYTES}>
              {custodyHeldBytesText(row.receipt, custodyDegradedBadgeKey())}
            </p>
            <!-- The honest bound, stated beside the control it bounds (REQUIRED
                 — `ui/nests.md` § Trust facet, custody rows). A pending ceremony
                 has minted nothing to revoke, so the note would over-promise
                 there and the control is disabled instead. -->
            {#if !row.pending}
              <p class="custody-bound">{t.devices.custody_revoke_bound_note}</p>
            {/if}
            <div class="device-actions">
              <button
                class="btn btn-small btn-danger"
                data-testid={IDS.CUSTODY_HOLDER_REVOKE_BUTTON}
                disabled={row.pending}
                onclick={() => revokeCustody(row)}
              >{t.devices.custody_revoke}</button>
            </div>
          </div>
        {/each}
      </div>
    </section>
  {/if}
{/if}

<style>
  h2 { font-size: 1.25rem; margin-bottom: 0.5rem; }
  .muted { color: var(--text-muted); }
  .section { margin-bottom: 2rem; }

  .identity-copy-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }
  .mono { font-family: monospace; font-size: 0.8rem; word-break: break-all; }

  .device-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 1rem;
  }
  .device-card {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1rem;
    background: var(--bg-surface);
  }
  .device-header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.5rem;
  }
  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--danger);
    flex-shrink: 0;
  }
  .status-dot.online { background: var(--success); }
  .device-meta {
    font-size: 0.8125rem;
    color: var(--text-muted);
    margin-bottom: 0.5rem;
  }
  .device-roles {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    margin-bottom: 0.5rem;
  }
  .badge {
    font-size: 0.75rem;
    padding: 0.125rem 0.5rem;
    border-radius: 4px;
    background: var(--bg-hover);
    color: var(--text-muted);
  }
  .guardian-badge {
    margin-left: auto;
    background: var(--accent, var(--primary, #4a5));
    color: #fff;
  }
  .this-device-badge {
    margin-left: auto;
    background: var(--primary, #47a);
    color: #fff;
  }
  /* A stale custodian reads as degraded redundancy the owner can SEE — styled
     as a warning rather than dimmed away (the A7 honesty rule). */
  .custody-stale { color: var(--danger); }
  .custody-bound {
    font-size: 0.75rem;
    color: var(--text-muted);
    margin-bottom: 0.5rem;
  }
  .own-fingerprint { font-size: 0.75rem; color: var(--text-muted); }
  .member-note {
    font-size: 0.8125rem;
    color: var(--text-muted);
    margin-bottom: 1rem;
  }
  .device-actions { margin-top: 0.5rem; display: flex; gap: 0.5rem; }
  .p2p-participation { margin-top: 0.5rem; display: flex; align-items: center; gap: 0.4rem; font-size: 0.85rem; }
  .p2p-participation[aria-disabled='true'] { opacity: 0.6; }
  .btn {
    padding: 0.5rem 0.875rem;
    border-radius: 6px;
    background: var(--bg-hover);
    color: var(--text);
    border: 1px solid var(--border);
    cursor: pointer;
  }
  .btn-small { font-size: 0.8125rem; padding: 0.375rem 0.625rem; }
  .btn-danger { color: var(--danger); border-color: var(--danger); }
</style>
