<script lang="ts">
  import { connectionStatus, identity } from '$lib/store';
  import {
    fetchMessageKindSnapshots,
    fetchRestoreHistory,
    fetchRestoreDivergence,
    restoreMessageKind,
    backupDestinationList,
    backupDestinationKeep,
    backupDestinationAdd,
    backupDestinationEdit,
    backupDestinationRemove,
    backupDestinationStatus,
    backupAuditRunPass,
    type DestinationAuditRow,
    type MessageKindSnapshot,
    type RestoreHistoryRow,
    type RestoreDivergenceRow,
    type BackupDestination,
    type BackupDestinationStatus,
  } from '$lib/rpc';
  import { nodeUrl } from '$lib/api';
  import { sharedRpcPort } from '$lib/rpc';
  import {
    ensureWasm,
    backupDestinationLabel,
    everyDestinationIsAClientDevice,
    hexShort,
    immediateDeleteAckText,
    downloadSnapshotFileBytes,
    offlineAffordance,
    backupSelfAuditIsAlerting,
    backupSelfReportedAlertReason,
    snapshotRestoreOptionLabel,
  } from '$lib/wasm';
  import { makeOfflineGate } from '$lib/offline-gate';
  import {
    createBackupsMachine,
    busyText as sharedBusyText,
    snapshotStateText,
    snapshotIntegrityText,
  } from '$lib/wasm-backups';
  import {
    snapshotLifecycle,
    type BackupOp,
    type BackupsSnapshot,
    type SnapshotFileRow,
    type SnapshotRow,
  } from '$lib/backups-machine';
  import { bytesFromHex } from '$lib/hex';
  import { resolveLocalized } from '$lib/i18n/localized';
  import type { BackupsMachine } from '../../../static/fauna_wasm_backups.js';
  import { onMount, onDestroy } from 'svelte';
  import { ensureAccountRuntime } from '$lib/conversations';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import {
    byteSize,
    backupLastUploadText,
    backupBacklogText,
    backupLastAuditText,
    backupSelfAuditText,
    backupAuditAlertText,
    backupDestinationKindText,
    backupUsageText,
  } from '$lib/value-format';
  import { auditAlertReasons } from '$lib/backup-audit-alerts';
  import { IDS } from '$lib/generated/uiIds';

  // W4 (account-data-plane.md § Workstreams) phase 4's UI desensitizing (`account-data-plane.md` § The
  // offline-mutation contract). The verdict is the shared rule's, never a class
  // test written here; `$lib/offline-gate` owns how a reactive tree obeys it.
  const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);

  let error = $state('');
  let ready = $state(false);

  // --- The snapshot half: a dumb renderer of the shared `BackupsMachine`
  // (`libs/fauna-backups-machine`, WASM twin `libs/fauna-wasm-backups`).
  // Architectural rule 1 binds from the machine's landing — this page paints
  // `snap` and dispatches gestures; it holds no page logic. What that retired
  // here, each a divergence `ui/backups.md` § Snapshot-list shape → *Reconciliation
  // ledger* measured on web:
  //
  //   * `last-backed-up` off `fauna.sync.backup_status`'s `last_change_at` — the
  //     last *file change*, not the last snapshot. A live wrong-value bug, and
  //     the reason that whole read (and its 10 s poll) is gone: nothing else
  //     consumed it.
  //   * the selector as N indexed buttons under one non-indexed id — now ONE
  //     `<select>`, which is what `backup-folder-selector` is everywhere else.
  //   * successes routed through the `error-message` banner (prune's "Pruned N",
  //     the integrity verdict) — a completed check with errors is a **result,
  //     not an error** (Architectural rule 6) and now renders on its own surface.
  //   * three hardcoded-English `window.confirm` / result strings.
  //   * the client-supplied `keep_last: 3` prune (and the wasm binding's
  //     hard-coded `dry_run = false`) — prune is preview-first over the set's own
  //     resting policy.
  // `$state`, not a plain `let`: the friction-bar predicate below is a `$derived`
  // that must re-evaluate once the machine exists (and TS narrows a plain
  // `let` initialised to `null` straight to `never` there).
  let machine = $state<BackupsMachine | null>(null);
  let snap = $state<BackupsSnapshot | null>(null);
  // Whether the banner currently shows the MACHINE's error. The destination half
  // and the restore half write the same `error-message` element, so a machine tick
  // must not silently clear one of their failures — this is which half owns the
  // text right now. Every non-machine writer goes through `showPageError` so the
  // ownership flag cannot be forgotten at a call site.
  let errorFromMachine = false;

  /** Show a failure from a half of this page the machine does not own (the
   *  destination CRUD, the restore action, the per-file download). */
  function showPageError(message: string): void {
    error = message;
    errorFromMachine = false;
  }

  /** Single-flight (§ *Create* ruling): while an op is in flight EVERY mutating
   *  control is disabled. One predicate so no control can drift off it. */
  let busy = $derived(snap?.in_progress_op != null);
  let armed = $derived(!busy && (snap?.selected_folder ?? null) !== null);

  // --- Restore surfaces (docs/goal/ui/backups.md §§ Restore history /
  // Restore divergence / Restore from backup destination). Mirror of the
  // Linux `views/backups/restore.rs` glue over the shared SnapshotsClient. ---
  let restoreSnapshots: MessageKindSnapshot[] = $state([]);
  let selectedRestoreId = $state(''); // stringified snapshot id of the picked row
  let restoreConfirm = $state(''); // friction-bar re-typed id
  let restoreKindMail = $state(true);
  let restoreKindCalendar = $state(true);
  // Explicit `string`: the progress line cycles through several i18n strings
  // (idle/running/done/no-snapshots); without the annotation TS narrows it to
  // the initial literal's type and rejects the other assignments.
  let restoreProgress = $state<string>(t.backups.restore_progress_idle);
  let restoring = $state(false);
  // `restore-warning`: the last restore's reply said config_present == false.
  // The reply is the advisory's only carrier, so this flag is the one copy.
  let restoreConfigAbsent = $state(false);
  let restoreHistory: RestoreHistoryRow[] = $state([]);
  // snapshot_id → divergence rows (only rows with ≥1 entry get a banner).
  let divergence: Record<number, RestoreDivergenceRow[]> = $state({});
  // The divergence-details modal, opened from a row's banner.
  let divergenceModalRows: RestoreDivergenceRow[] | null = $state(null);

  // --- Backup destinations (docs/goal/ui/backups.md § Manage backup
  // destinations). The web twin of the Linux views/backups/destinations.rs
  // glue: add resolves identity + records via fauna.account.state.put, edit
  // renames/re-points (same-nest only), remove is a plain config edit. ---
  // Retry budget for the destination read — the page can mount before the WS is
  // ready, which is a timing artefact rather than a real "you have none".
  const DESTINATION_LOAD_ATTEMPTS = 3;
  const DESTINATION_LOAD_RETRY_MS = 400;
  let destinations: BackupDestination[] = $state([]);
  let showDestForm = $state(false);
  let editingDestId: string | null = $state(null); // null while adding
  let destUrl = $state('');
  let destName = $state('');
  let destBusy = $state(false); // a resolve/persist round-trip is in flight
  let removingDestId: string | null = $state(null); // armed remove-confirm
  // Live per-destination status (backups.md § Per-destination status read),
  // keyed by destination_id. A destination with no entry (failed/not-yet read)
  // renders the not-yet-backed-up baseline ("never" / "0 queued"), mirroring
  // linux's `unwrap_or_default()` degrade.
  let destStatus: Record<string, BackupDestinationStatus> = $state({});
  // The last audit pass's per-destination records, keyed by destination_id
  // (backups.md § Audit-alert surface). Cached because the audit and the status
  // read are **independent** round trips — see `refreshAudit` — so whichever lands
  // first must not blank the other. Empty until the first pass returns, which
  // renders "never" per the shared label's `None` arm.
  let destAudit: Record<string, DestinationAuditRow> = $state({});

  // The restore button arms only when the typed id exactly matches the
  // selected snapshot's id (same friction bar as the immediate-delete modal).
  let restoreButtonEnabled = $derived(
    !restoring && restoreConfirm !== '' && restoreConfirm === selectedRestoreId,
  );

  // --- Immediate-delete modal (docs/goal/ui/backups.md § Element IDs + the
  // friction-bar behavioural invariant, rule 4 line 310). The owner-only
  // delete_immediate override: NEVER a one-click affordance. The per-row
  // snapshot-immediate-delete-button only OPENS the modal; confirm dispatches
  // the machine's gesture. ---
  //
  // The id the open modal targets, or null when closed. The RENDER PASS closes
  // it, keyed on the row leaving the machine's list — never on the call
  // returning: a `hard_floor_breach` refusal returns from the same call and must
  // leave the modal, the typed inputs and the row exactly where the user left
  // them (linux's leg paid for this one).
  let immediateDeleteTargetId = $state<number | null>(null);
  let immediateDeleteConfirmId = $state('');
  let immediateDeleteAcknowledge = $state('');
  // The exact acknowledge phrase the nest checks byte-for-byte — the protocol
  // constant over wasm (NOT i18n), displayed for the user to re-type. Set after
  // ensureWasm() in onMount (so wasm() is initialized before the read).
  let immediateDeleteAck = $state('');

  // The confirm button enables ONLY when BOTH inputs match exactly: the typed id
  // == the snapshot id retyped AND the typed acknowledge == the protocol phrase
  // (backups.md rule 4). The MACHINE's predicate, with its real in-flight flag
  // threaded in — never a local re-derivation (Architectural rule 4). Depends on
  // `snap` so an in-flight op re-evaluates it.
  let immediateDeleteButtonEnabled = $derived(
    snap != null &&
      machine != null &&
      immediateDeleteTargetId != null &&
      machine.immediateDeleteEnabled(
        immediateDeleteConfirmId,
        String(immediateDeleteTargetId),
        immediateDeleteAcknowledge,
      ),
  );

  /** Re-read the whole page state from the machine and re-point the banner.
   *  Runs on every observer tick. */
  function applySnapshot(): void {
    if (!machine) return;
    const raw = machine.snapshotJson();
    snap = raw ? (JSON.parse(raw) as BackupsSnapshot) : null;

    const machineText = snap?.error ? resolveLocalized(snap.error) : '';
    if (machineText) {
      error = machineText;
      errorFromMachine = true;
    } else if (errorFromMachine) {
      error = '';
      errorFromMachine = false;
    }

    closeImmediateDeleteModalIfLanded();
  }

  /** Close the friction-bar modal once the row it targets has left the machine's
   *  list — i.e. the delete actually landed. An in-flight op or a live error both
   *  mean "not landed", so neither closes it. */
  function closeImmediateDeleteModalIfLanded(): void {
    const target = immediateDeleteTargetId;
    if (target == null || !snap) return;
    if (snap.in_progress_op != null || snap.error != null) return;
    if (snap.snapshots.some((s) => s.id === target)) return;
    immediateDeleteTargetId = null;
  }

  // The path currently downloading — one at a time, so the row's button can
  // disable itself without a per-row flag.
  let downloadingPath = $state('');

  /**
   * Download one snapshot file's bytes and save them.
   *
   * The bytes are resolved + decrypted entirely client-side by the shared walk
   * (`downloadSnapshotFileBytes` → `fauna_core::file_download` over wasm). This
   * page cannot ask the nest to reassemble the file: owner chunks are sealed and
   * the nest holds no opening key.
   * Only the save below is web-specific (`ui/backups.md` § Where logic lives).
   */
  async function downloadSnapshotFile(file: SnapshotFileRow) {
    const id = $identity;
    if (!id) return;
    // The machine hands `manifest_hash` across the boundary as hex (the same
    // reason `SnapshotRow.device_id` is hex); the download walk wants bytes.
    const manifestHash = bytesFromHex(file.manifest_hash);
    if (!manifestHash) {
      showPageError(t.backups.error_download_manifest);
      return;
    }
    downloadingPath = file.path;
    try {
      const bytes = await downloadSnapshotFileBytes(
        manifestHash,
        file.path,
        id.secretHex,
        nodeUrl(),
      );
      const url = URL.createObjectURL(new Blob([bytes as BlobPart]));
      try {
        const a = document.createElement('a');
        a.href = url;
        a.download = file.path.split('/').pop() ?? file.path;
        a.click();
      } finally {
        URL.revokeObjectURL(url);
      }
    } catch (e: any) {
      showPageError(e.message);
    }
    downloadingPath = '';
  }

  // ── Snapshot-half gestures ───────────────────────────────────────────
  // Each is one machine call; the observer tick that follows repaints. No
  // client-side re-fetch and no local busy flag — `in_progress_op` is the
  // single-flight gate for all of them, and the machine owns the error.

  async function selectFolder(name: string) {
    await machine?.selectFolder(name);
  }

  async function triggerSnapshot() {
    await machine?.createSnapshot();
  }

  function goBack() {
    machine?.closeSnapshotDetail();
  }

  async function loadRestoreSnapshots() {
    const id = $identity;
    if (!id) return;
    try {
      restoreSnapshots = await fetchMessageKindSnapshots(id.secretHex);
      // Auto-select the newest (index 0), matching the Linux dropdown default.
      if (restoreSnapshots.length > 0) {
        selectedRestoreId = String(restoreSnapshots[0].id);
      } else {
        selectedRestoreId = '';
        restoreProgress = t.backups.restore_no_snapshots;
      }
    } catch (e: any) {
      showPageError(e.message);
    }
  }

  async function loadRestoreHistory() {
    const id = $identity;
    if (!id) return;
    try {
      restoreHistory = await fetchRestoreHistory(id.secretHex);
      // Fetch each row's divergence rows; a row only gets a banner when ≥1.
      const next: Record<number, RestoreDivergenceRow[]> = {};
      await Promise.all(
        restoreHistory.map(async (row) => {
          try {
            const rows = await fetchRestoreDivergence(id.secretHex, row.snapshot_id);
            if (rows.length > 0) next[row.snapshot_id] = rows;
          } catch {
            // Divergence read is advisory; a failure just omits the banner.
          }
        }),
      );
      divergence = next;
    } catch (e: any) {
      showPageError(e.message);
    }
  }

  function restoreHistoryRowText(row: RestoreHistoryRow): string {
    // source_member_id null → "local snapshot" (Plan 4 populates it).
    const source =
      row.source_member_id == null
        ? t.backups.restore_source_local
        : hexShort(row.source_member_id);
    return t.backups.restore_history_row({
      kinds: row.kinds_restored,
      source,
      when: formatTime(row.completed_at),
    });
  }

  async function triggerRestore() {
    const id = $identity;
    if (!id || !restoreButtonEnabled) return;
    const snapshotId = Number(selectedRestoreId);
    restoring = true;
    restoreProgress = t.backups.restore_progress_running;
    restoreConfigAbsent = false;
    try {
      const reply = await restoreMessageKind(id.secretHex, snapshotId, restoreConfirm);
      // Set together with DONE, so a reader that sees DONE sees the verdict.
      restoreConfigAbsent = !reply.config_present;
      restoreProgress = t.backups.restore_progress_done;
      restoreConfirm = '';
      // The restore wrote a restore_history row — reflect it.
      await loadRestoreHistory();
    } catch (e: any) {
      showPageError(e.message);
      restoreProgress = t.backups.restore_progress_idle;
    }
    restoring = false;
  }

  function openDivergenceModal(snapshotId: number) {
    divergenceModalRows = divergence[snapshotId] ?? [];
  }

  function divergenceDetailText(row: RestoreDivergenceRow): string {
    return t.backups.restore_divergence_detail_row({
      collection: row.collection,
      mua: row.mua_id ?? t.backups.restore_divergence_unknown_mua,
      client: String(row.client_modseq),
      server: String(row.server_modseq),
      lost: String(row.lost_event_count),
    });
  }

  onMount(async () => {
    // The destination list is an account-plane row (`fauna.state.backup`), so
    // this page needs the tab's account runtime even when no conversations
    // page has been opened yet.
    ensureAccountRuntime();
    // FIRST, before any await: the `backup_audit_run_now` agent command drives
    // *this* — the production audit path, same call and same render, not a
    // test-only shortcut. Installed up front so "the page is mounted" (which is all
    // a driver's `navigate()` can observe) implies "the hook is there"; installing
    // it after the page's several load round-trips would make the command's
    // availability depend on wall-clock timing, which convention 14 forbids. Test
    // builds only (testing.md § convention 15): a production `vite build` folds the
    // constant to false and strips the import.
    if (__FAUNA_E2E_AUTOMATION__) {
      const { setBackupAuditRerunHook } = await import('$lib/backup-audit-e2e');
      setBackupAuditRerunHook(refreshAudit);
    }
    await ensureWasm();
    // The byte-for-byte ack phrase the immediate-delete modal makes the user
    // re-type — read once wasm is up (the modal can't open before `ready`).
    immediateDeleteAck = immediateDeleteAckText();
    identity.init();
    const id = $identity;
    if (id) {
      try {
        machine = await createBackupsMachine(
          { onChanged: () => applySnapshot() },
          // The SPA singleton's socket, lent to the backups chunk — one
          // WebSocket per actor.
          await sharedRpcPort(id.secretHex),
          // The owner secret: what the machine derives its label-opening
          // `BackupKey` from. Without it a sealed set's file list renders empty
          // for a reader who holds the key.
          id.secretHex,
        );
      } catch (e: unknown) {
        showPageError(e instanceof Error ? e.message : String(e));
      }
    }
    ready = true;
    // One refresh populates the selector AND selects the first set (the machine's
    // deterministic default) — which is what retired the page's own single-set
    // auto-select and its 10 s `fauna.sync.backup_status` poll.
    await machine?.refresh();
    applySnapshot();
    await loadRestoreSnapshots();
    await loadRestoreHistory();
    await loadDestinations();
    await loadDestinationStatus();
    // The audit is fired last and NOT awaited into the mount path's critical
    // section: it opens the client's own session to every destination, so a dead
    // one would otherwise delay every other row on the page. Cheap to fire on each
    // mount — the shared 24 h `AUDIT_MIN_INTERVAL` debounce makes a repeat visit
    // one `localStorage` read and no round trip.
    void refreshAudit();
  });

  onDestroy(() => {
    // Drop the rerun hook with the page, so the command fails loudly ("the Backups
    // page is not mounted") instead of poking a torn-down closure.
    if (__FAUNA_E2E_AUTOMATION__) {
      void import('$lib/backup-audit-e2e').then((m) => m.setBackupAuditRerunHook(null));
    }
  });

  async function handleDeleteSnapshot(snapId: number) {
    // The confirm is client glue (the four apps that ship one keep theirs); the
    // machine queues the 48 h pending action and the row returns as
    // `DeletionPending`.
    if (!confirm(t.backups.delete_snapshot_confirm)) return;
    await machine?.deleteSnapshot(snapId);
  }

  // Recovery is offered ONLY out of `SoftDeleted` (backups.md § *Soft-deleted
  // rows*) — the machine refuses the gesture for any other row, so the render
  // guard below is the affordance rule, not the enforcement.
  async function handleUndeleteSnapshot(snapId: number) {
    await machine?.undeleteSnapshot(snapId);
  }

  // --- Immediate-delete handlers (backups.md § User actions) ---

  function openImmediateDelete(row: SnapshotRow) {
    immediateDeleteTargetId = row.id;
    immediateDeleteConfirmId = '';
    immediateDeleteAcknowledge = '';
  }

  function cancelImmediateDelete() {
    immediateDeleteTargetId = null;
  }

  async function confirmImmediateDelete() {
    const target = immediateDeleteTargetId;
    if (target == null || !immediateDeleteButtonEnabled) return;
    // The modal deliberately stays open across this call: the render pass closes
    // it when the row leaves the machine's list, so the nest's `hard_floor_breach`
    // refusal leaves the friction bar and its typed inputs standing for a retry.
    await machine?.deleteSnapshotImmediate(
      target,
      immediateDeleteConfirmId,
      immediateDeleteAcknowledge,
    );
  }

  /** Prune OPENS the dry-run preview; nothing is deleted until the user executes
   *  from it (§ *Prune* ruling — apple's preview-first flow, blessed as the
   *  uniform shape). The old confirm dialog is retired along with the
   *  `keep_last: 3` policy it confirmed — this page never supplies a policy
   *  (Architectural rule 5). */
  async function handlePrune() {
    await machine?.prunePreview();
  }

  async function executePrune() {
    await machine?.pruneExecute();
  }

  function cancelPrune() {
    machine?.cancelPrunePreview();
    applySnapshot();
  }

  /** Clicking the tagged button RUNS the check — no second confirm step
   *  (§ *Check* ruling). The verdict renders on its own surface, never in
   *  `error-message` (Architectural rule 6). */
  async function handleCheck() {
    await machine?.check();
  }

  function formatTime(epoch: number | null): string {
    if (epoch == null) return 'Never';
    return new Date(epoch * 1000).toLocaleString();
  }

  // ── The snapshot half's render helpers ───────────────────────────────

  /** `last-backed-up` — ONE non-indexed element = the SELECTED set's newest
   *  snapshot `created_at`, "never" when it has none. Always painted, so an
   *  empty set can never leave the previous set's timestamp standing. */
  let lastBackedUpText = $derived(
    snap?.last_backed_up != null
      ? t.backups.last_backed_up_at({ when: formatTime(snap.last_backed_up) })
      : t.backups.last_backed_up_never,
  );

  /** The `in_progress_op` line — names which op is running so the disabled
   *  action row above it is not left unexplained. Off the shared
   *  `fauna_backups_machine::busy_text` (`docs/goal/ui/backups.md` § Where
   *  logic lives) — no app hand-rolls which key an operation maps to. */
  function busyText(op: BackupOp): string {
    return resolveLocalized(sharedBusyText(op));
  }

  /** The check verdict — the shared `is_ok` predicate, **called**, never
   *  re-derived from `status === 'ok'` or from the error counts. */
  let checkResultText = $derived.by(() => {
    const r = snap?.check_result;
    if (!r) return '';
    return r.is_ok
      ? t.backups.check_result_ok({
          snapshots: String(r.snapshots_checked),
          files: String(r.files_checked),
          chunks: String(r.chunks_checked),
        })
      : t.backups.check_result_errors({
          missing_manifests: String(r.missing_manifests),
          missing_chunks: String(r.missing_chunks),
          corrupt_manifests: String(r.corrupt_manifests),
        });
  });

  /** A preview with candidates is what arms execute — an armed execute over zero
   *  candidates would promise an effect it cannot have, and the two no-op policy
   *  states say WHY nothing would be pruned instead of showing an empty success. */
  let pruneExecutable = $derived(
    !busy &&
      snap?.prune_preview?.policy_state === 'Applied' &&
      (snap?.prune_preview?.candidates.length ?? 0) > 0,
  );

  /** The visible row text: timestamp + file count + formatted size, plus the
   *  non-`Active` lifecycle state with the deadline the user can still act on,
   *  and — once a check has run this session — the derived integrity verdict.
   *  The lifecycle + integrity suffixes route through the shared
   *  `fauna_backups_machine::{snapshot_state_text, snapshot_integrity_text}`
   *  (`docs/goal/ui/backups.md` § Where logic lives) — this page owns only
   *  the timestamp formatting; `snapshotLifecycle` still owns decoding the
   *  wire's externally-tagged union (it also guards the undelete button). */
  function snapshotRowText(row: SnapshotRow): string {
    let text = `${formatTime(row.created_at)} · ${t.backups.file_count({
      count: String(row.file_count),
    })} · ${byteSize(row.total_bytes)}`;
    const lifecycle = snapshotLifecycle(row.state);
    const formattedDeadline =
      lifecycle.kind !== 'active' && lifecycle.deadline != null
        ? formatTime(lifecycle.deadline)
        : null;
    const stateText = snapshotStateText(row.state, formattedDeadline);
    if (stateText) text += `  ${resolveLocalized(stateText)}`;
    const integrityText = snapshotIntegrityText(row.integrity);
    if (integrityText) text += `  ${resolveLocalized(integrityText)}`;
    return text;
  }

  // --- Backup-destination handlers ---

  // A failed destination read is **unknown**, not empty — and on THIS page that
  // distinction is the whole point. Swallowing it into `[]` tells an owner who
  // has destinations configured that they have none, on the one page whose job
  // is to say whether their data is being backed up anywhere, and it makes a
  // genuine load failure indistinguishable from a fresh account.
  //
  // ⚠ This is not a new ruling — **linux already made it and fixed this exact
  // bug** (`apps/fauna-linux/src/views/backups/destinations.rs`, the
  // `StatusLoad::failed(e)` arm: *"A failed read is unknown, not empty.
  // Swallowing it here (the pre-fix `unwrap_or_default`) told an owner with
  // destinations configured that they had none"*). Web's old comment here cited
  // that same `unwrap_or_default` as its justification, having copied linux's
  // shape from **before** the fix. Priority #4: resolve the drift onto the
  // richer pattern rather than keep matching the older one.
  //
  // The mount-before-WS-ready case the old comment worried about is real, and
  // linux answers it with a retry rather than by lying: retry first, and only
  // report once the retries are spent. `hydrated` is what separates "we have not
  // successfully read yet" from "we read, and it is empty", so the template can
  // render the empty placeholder ONLY in the second case.
  let destinationsHydrated = $state(false);

  async function loadDestinations() {
    const id = $identity;
    if (!id) return;
    let lastError: unknown = null;
    for (let attempt = 0; attempt < DESTINATION_LOAD_ATTEMPTS; attempt++) {
      try {
        destinations = await backupDestinationList(id.secretHex);
        destinationsHydrated = true;
        return;
      } catch (e: unknown) {
        lastError = e;
        if (attempt + 1 < DESTINATION_LOAD_ATTEMPTS) {
          await new Promise((r) => setTimeout(r, DESTINATION_LOAD_RETRY_MS));
        }
      }
    }
    // Retries spent. Say so rather than rendering a confident empty list: the
    // rows we hold are stale-or-absent and we do not know which.
    destinationsHydrated = false;
    showPageError(
      lastError instanceof Error ? lastError.message : String(lastError),
    );
  }

  // Live per-destination status (backups.md § Per-destination status read).
  // Re-read on mount and after every add/edit/remove. Skipped at zero
  // destinations (no fauna.segments.list round-trip — the spec's skip); a read
  // failure degrades silently to the baseline (empty map → "never" / "0
  // queued"), mirroring linux's `read_destination_status(...).unwrap_or_default()`.
  async function loadDestinationStatus() {
    const id = $identity;
    if (!id || destinations.length === 0) {
      destStatus = {};
      return;
    }
    try {
      const rows = await backupDestinationStatus(id.secretHex);
      const next: Record<string, BackupDestinationStatus> = {};
      for (const r of rows) next[r.destination_id] = r;
      destStatus = next;
    } catch {
      destStatus = {};
    }
  }

  // `backup-destination-last-upload-time` / `-backlog-count` text. The never-vs-real key
  // selection, the epoch-`0` guard, the seconds→ms conversion, and the absent-status 0
  // baseline all live in shared Rust (`fauna_core::format`), reached through
  // `$lib/value-format` — the single web surface for these. No status entry = a
  // not-yet/failed read, which carries the same "never"/0 baseline as an empty one.
  function lastUploadText(destId: string): string {
    return backupLastUploadText(destStatus[destId]?.last_upload_time ?? null);
  }

  function backlogText(destId: string): string {
    return backupBacklogText(destStatus[destId]?.backlog_count ?? null);
  }

  // --- The client-side audit (backups.md § Audit-alert surface) ---
  //
  // Deliberately a SEPARATE round trip from `loadDestinationStatus`, matching
  // linux: the audit opens the client's own authenticated session to every
  // destination and samples bytes from it, so a slow or dead destination must never
  // hold up the status rows. Two independent reads, each rendering as it lands.
  //
  // Web implements NO audit logic. `backupAuditRunPass` is one call into shared
  // Rust that loads this device's state, audits everything due, merges over what
  // was already known, persists, and hands back one row per configured
  // destination — merge, 24 h debounce and verdict all shared. A failed pass
  // degrades silently to the previous cache: a client that cannot reach a
  // destination has nothing the user can act on *yet*, and the shared loop's own
  // `Unreachable`→`Overdue` escalation is what eventually makes it loud.
  async function refreshAudit() {
    const id = $identity;
    if (!id) return;
    try {
      const rows = await backupAuditRunPass(id.secretHex);
      const next: Record<string, DestinationAuditRow> = {};
      for (const r of rows) next[r.destination_id] = r;
      destAudit = next;
    } catch {
      // Keep the prior records: dropping them would clear a standing banner on a
      // transient failure, which is `merge_outcomes`' whole reason for existing.
    }
  }

  // `backup-destination-last-audit-time` text for a row: when this client's own
  // audit last **passed**. `undefined` (no pass yet, or no audit has run this page
  // visit) ⇒ "never" via the shared label's own `None` arm.
  //
  // Note what this is *not*: the row above it (`last_upload_time`) is the source
  // nest reporting on its own uploads; this is the only line on the page that
  // neither the source nor the destination gets to assert.
  function lastAuditText(destId: string): string {
    return backupLastAuditText(destAudit[destId]?.last_passed_at ?? null, Date.now());
  }

  // `backup-destination-last-audit-time` text for a **client-device
  // custodian** row: the device's own self-audit, off the status row's
  // `last_audit_passed_at` rather than an audit record (`backups.md`
  // § Audit-alert surface → *The client-device arm*). A custodian has no
  // address for the owner-side loop above to reach.
  function selfAuditText(destId: string): string {
    return backupSelfAuditText(destStatus[destId]?.last_audit_passed_at ?? null, Date.now());
  }

  // The failing destinations, in `destinations` order so banner order and row order
  // agree without sorting anything (the shared `merge_outcomes` already returns the
  // records in that order). A destination with no `alert_reasons` is healthy —
  // *which* reasons are loud (the standing verdict's, then an open source-
  // regression recovery window) is `DestinationAuditRecord::alert_reasons()`'s
  // single shared answer, never re-derived here, so web cannot start alerting on
  // a transient `Unreachable` (the laptop-on-a-plane case the loop keeps quiet).
  let auditAlerts = $derived([
    ...destinations.flatMap((d) =>
      auditAlertReasons(destAudit[d.destination_id]).map((reason) =>
        backupAuditAlertText(reason, destLabel(d)),
      ),
    ),
    // A client-device custodian reporting its OWN copy as failing — the only
    // failure signal that exists for a kind the owner-side loop can never
    // sample (`backup-destinations.md` § Custodian contract, question 4).
    // Which reported states are loud stays single-sourced in the shared
    // `backupSelfAuditIsAlerting`.
    ...destinations
      .filter((d) => backupSelfAuditIsAlerting(destStatus[d.destination_id]?.audit_state ?? null))
      .map((d) => backupAuditAlertText(backupSelfReportedAlertReason(), destLabel(d))),
  ]);

  // Row label: the display name, else the destination URL's host (an unnamed destination
  // carries null). Shared `fauna_core::format` over wasm
  // (`backup_destination_label`) — one source of truth across the six apps.
  function destLabel(d: BackupDestination): string {
    return backupDestinationLabel(d.display_name ?? null, d.destination_nest_url);
  }

  // --- The client-device destination kind (backups.md § Third destination kind) ---
  //
  // Web renders these rows; it deliberately does **not** offer to become one.
  // Enrollment runs ON the device being enrolled (§ Third destination kind →
  // Enrollment), and a browser tab cannot host the sealed store the kind exists
  // to provide: the shared pull coordinator is native-only and cannot compile to
  // wasm (rusqlite, a !Sync Connection, tokio::spawn — the decisive 2026-06-18
  // scoping). So `backup-destination-kind-select` / `-capacity-input` are absent
  // here — an optional_elements absence, declared in backups.md § Implementation
  // status today — while the read-side badge, usage and sole-client warning all
  // render, because the rows themselves arrive from the owner's OTHER devices
  // and are exactly what this page must report honestly.

  // `backup-destination-kind-badge` text. Reads the row's own discriminator
  // through the shared label, so a kind a newer client wrote renders as itself
  // rather than masquerading as a nest.
  function kindBadgeText(d: BackupDestination): string {
    return backupDestinationKindText(d.kind);
  }

  // Is this row one of the owner's own devices? The typed answer is the shared
  // rule's (`row_is_a_client_device` behind `everyDestinationIsAClientDevice`),
  // and it is what gates `backup-destination-usage` — a nest row has no cap and
  // no held-bytes report, so the element is ABSENT there rather than empty.
  // Reusing the sole-client predicate over a one-row array keeps this page from
  // string-matching `kind`, which is how the Inert arm gets lost.
  function isClientDevice(d: BackupDestination): boolean {
    return everyDestinationIsAClientDevice([d]);
  }

  // `backup-destination-usage` text: held bytes against the user-set cap.
  // `cap_state` rides through untouched — cap-reached is read, never inferred
  // (a pass that stopped AT its cap ends below it, so `held >= cap` would render
  // a stalled backup as healthy-with-room).
  function usageText(d: BackupDestination): string {
    const s = destStatus[d.destination_id];
    return backupUsageText(
      s?.held_bytes ?? null,
      d.capacity_cap_bytes ?? null,
      s?.cap_state ?? null,
    );
  }

  // `backup-sole-client-destination-warning` — the standing durability warning,
  // painted only while EVERY configured destination is one of the owner's own
  // devices. The predicate is shared, not re-derived: its empty-list arm (an
  // account with no backup at all is not "sole-client") and its Inert arm (an
  // unrecognised kind counts as NOT a client device, since it may well BE the
  // off-site copy) are policy answers, not rendering ones.
  //
  // Gated on `ready` (set after `ensureWasm()`) rather than on the list being
  // non-empty: this derived evaluates at component init, when `wasm()` still
  // throws. Short-circuiting on `destinations.length` would have worked by
  // accident AND quietly re-implemented the predicate's empty-list arm here —
  // the one arm the shared side owns because it is a policy answer.
  let soleClientDestinations = $derived(ready && everyDestinationIsAClientDevice(destinations));

  function openAddDest() {
    editingDestId = null;
    destUrl = '';
    destName = '';
    removingDestId = null;
    showDestForm = true;
  }

  function openEditDest(d: BackupDestination) {
    editingDestId = d.destination_id;
    destUrl = d.destination_nest_url;
    destName = d.display_name ?? '';
    removingDestId = null;
    showDestForm = true;
  }

  function cancelDest() {
    showDestForm = false;
    editingDestId = null;
  }

  /** Mirror of `fauna_ffi::backup_destinations::EDIT_DIFFERENT_NEST_ERR` — the
   * sentinel the shared FFI returns when an edit repoints a destination at a
   * different nest, so each app localizes it (android/apple/windows do the
   * same). i18n stays in the view. */
  const EDIT_DIFFERENT_NEST_ERR = 'backup-destination-edit-different-nest';

  async function submitDest() {
    const id = $identity;
    if (!id || destUrl.trim() === '') return;
    destBusy = true;
    error = '';
    try {
      const editing = editingDestId;
      // ⚠ The mutation's own returned list is deliberately DISCARDED: it carries
      // the stored rows without `unattested`, so assigning it would silently
      // un-paint the review mark on every *other* raised row until the next
      // mount. Only `backupDestinationList` resolves the mark plane, so the
      // re-read below is what keeps one source of truth for the rows on screen.
      if (editing) {
        await backupDestinationEdit(id.secretHex, editing, destUrl.trim(), destName.trim());
      } else {
        await backupDestinationAdd(id.secretHex, destUrl.trim(), destName.trim());
      }
      await loadDestinations();
      showDestForm = false;
      editingDestId = null;
      await loadDestinationStatus();
      // Re-audit after a mutation: a new destination gets its "never" record so the
      // row renders rather than vanishing until the first pass, and an edited or
      // re-pointed one is judged against where it now points.
      await refreshAudit();
    } catch (e: any) {
      const msg = e.message ?? String(e);
      showPageError(
        msg.includes(EDIT_DIFFERENT_NEST_ERR)
          ? t.backups.backup_destination_edit_different_nest
          : msg,
      );
    } finally {
      destBusy = false;
    }
  }

  /** **Keep** — "I recognise this". Records the verdict at rest and leaves the
   *  destination enrolled; the mark then stops rendering because the re-read
   *  below resolves `unattested: false` for that row.
   *
   *  No confirm, by the rule stated in the markup: Keep is non-destructive and
   *  re-decidable. The write itself is the shared
   *  `keep_backup_destination_at_rest` over wasm, so it rides the CAS + merge
   *  path and cannot clobber a concurrent device's adjudication. */
  async function keepDest(destId: string) {
    const id = $identity;
    if (!id) return;
    destBusy = true;
    try {
      await backupDestinationKeep(id.secretHex, destId);
      error = '';
      // Re-read rather than clear the flag locally: the verdict is at rest, and
      // the page must render what the account says, not what this tab hoped.
      // ⚠ AFTER the clear above, never before: `loadDestinations` reports a
      // spent-retries read through `showPageError`, and clearing afterwards
      // would wipe the one message saying the rows on screen are unknown.
      await loadDestinations();
    } catch (e: any) {
      showPageError(e.message ?? String(e));
    } finally {
      destBusy = false;
    }
  }

  function armRemoveDest(destId: string) {
    removingDestId = destId;
    showDestForm = false;
  }

  function cancelRemoveDest() {
    removingDestId = null;
  }

  async function confirmRemoveDest() {
    const id = $identity;
    const target = removingDestId;
    if (!id || !target) return;
    destBusy = true;
    try {
      // Same reason as `submitDest`: re-read rather than assign, so the rows
      // that REMAIN keep their review marks. The clear stays AHEAD of the
      // re-read for `keepDest`'s reason — a failed re-read must keep its say.
      await backupDestinationRemove(id.secretHex, target);
      error = '';
      await loadDestinations();
      await loadDestinationStatus();
      // Removing a destination must also clear its banner — an alarm the user
      // cannot dismiss is noise. `merge_outcomes` rule 2 drops the record of a
      // destination no longer configured, so the re-audit is what clears it.
      await refreshAudit();
    } catch (e: any) {
      showPageError(e.message ?? String(e));
    } finally {
      destBusy = false;
      removingDestId = null;
    }
  }

</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.backups.title}</h1>

<!-- The three set-scoped actions. All three bind the SAME `armed` predicate —
     single-flight (§ *Create* ruling): while any op is in flight every mutating
     control is disabled, and with no set selected there is nothing to act on. -->
<div class="page-actions">
  <button
    data-testid={IDS.SNAPSHOT_CREATE_BUTTON}
    class="btn"
    onclick={triggerSnapshot}
    disabled={!armed}
  >{t.backups.create_snapshot}</button>
  <button
    data-testid={IDS.SNAPSHOT_PRUNE_BUTTON}
    class="btn"
    onclick={handlePrune}
    disabled={!armed}
  >{t.backups.prune_snapshots}</button>
  <button
    data-testid={IDS.SNAPSHOT_CHECK_BUTTON}
    class="btn"
    onclick={handleCheck}
    disabled={!armed}
  >{t.backups.check_integrity}</button>
</div>

<MessageBanner bind:error />

<!-- Audit-alert banners (docs/goal/ui/backups.md § Audit-alert surface). Mounted
     ABOVE the snapshot list on purpose: a warning that a backup is not keeping up
     must be visible without scrolling past the snapshot half. Indexed — one per
     destination in a failing state, and rendered only while a failure stands (the
     `restore-divergence-banner` idiom). The text names both the destination and the
     reason, from the shared `backup_audit_alert_label`; which verdicts are loud is
     `DestinationAuditRecord::alert_reason()`'s answer, not this page's. -->
{#each auditAlerts as alert}
  <p data-testid={IDS.BACKUP_AUDIT_ALERT} class="banner audit-alert">{alert}</p>
{/each}

<div data-testid={IDS.SNAPSHOT_LIST}>
{#if !ready}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.identity_required}</p>
{:else}
  <!-- Folder selector — ONE `<select>` over the machine's `fauna.folders.list`
       rows (the ratified selector source: it carries the cached counts, and
       `backup_status`'s `last_change_at` was the last *file change*, not the last
       snapshot). This retires web's N indexed buttons under one non-indexed id:
       every other app renders one picker, and so does this now. A disabled control
       the user can see states why (Copy comprehensibility rule 5). -->
  <div class="folder-row">
    <label for="backup-folder-select">{t.backups.folder}</label>
    <select
      id="backup-folder-select"
      data-testid={IDS.BACKUP_FOLDER_SELECTOR}
      value={snap?.selected_folder ?? ''}
      disabled={(snap?.folders.length ?? 0) === 0 || busy}
      onchange={(e) => selectFolder((e.currentTarget as HTMLSelectElement).value)}
    >
      {#each snap?.folders ?? [] as fs}
        <option value={fs.name}>{fs.name}</option>
      {/each}
    </select>
    {#if (snap?.folders.length ?? 0) === 0}
      <span class="muted">{t.common.no_folders_configured}</span>
    {/if}
  </div>

  <!-- ONE non-indexed element, always painted: the selected set's newest snapshot,
       or "never". An empty set can therefore never leave the previous set's
       timestamp standing (web's old per-set indexed spans could, and read from the
       wrong source besides). -->
  <p class="last-backed-up" data-testid={IDS.LAST_BACKED_UP}>{lastBackedUpText}</p>

  {#if snap?.in_progress_op}
    <p class="muted caption">{busyText(snap.in_progress_op)}</p>
  {/if}

  <!-- The check verdict. A completed check WITH errors is a result, not an error
       (Architectural rule 6) — it renders here, never on `error-message`. -->
  {#if checkResultText}
    <p class="muted caption check-result" data-testid={IDS.SNAPSHOT_CHECK_RESULT}>
      {checkResultText}
    </p>
  {/if}

  <!-- The prune dry-run surface. Execute is offered ONLY from here, and the two
       no-op policy states say WHY nothing would be pruned. -->
  {#if snap?.prune_preview}
    <div class="prune-preview" data-testid={IDS.SNAPSHOT_PRUNE_PREVIEW}>
      <h3>{t.backups.prune_preview_title}</h3>
      {#if snap.prune_preview.policy_state === 'NotSet'}
        <p class="muted caption">{t.backups.prune_policy_not_set}</p>
      {:else if snap.prune_preview.policy_state === 'Unparseable'}
        <p class="caption warn">{t.backups.prune_policy_unparseable}</p>
      {:else if snap.prune_preview.candidates.length === 0}
        <p class="muted caption">{t.backups.prune_preview_nothing}</p>
      {:else}
        <p class="muted caption">
          {t.backups.prune_preview_counts({
            would_prune: String(snap.prune_preview.would_prune),
            remaining: String(snap.prune_preview.remaining),
          })}
        </p>
        {#each snap.prune_preview.candidates as candidate}
          <p class="muted caption">
            {t.backups.prune_preview_candidate({
              id: String(candidate.id),
              when: formatTime(candidate.created_at),
            })}
          </p>
        {/each}
      {/if}
      <div class="prune-preview-actions">
        {#if pruneExecutable}
          <button
            class="btn danger"
            data-testid={IDS.SNAPSHOT_PRUNE_EXECUTE_BUTTON}
            onclick={executePrune}>{t.backups.prune_execute_button}</button
          >
        {/if}
        <button class="btn" data-testid={IDS.SNAPSHOT_PRUNE_CANCEL_BUTTON} onclick={cancelPrune}
          >{t.backups.prune_cancel_button}</button
        >
      </div>
    </div>
  {/if}

  {#if snap?.detail}
    <button class="back" onclick={goBack}>&larr; {t.common.back}</button>
    <h2>{t.backups.snapshot({ id: String(snap.detail.snapshot_id) })}</h2>
    <table data-testid={IDS.SNAPSHOT_DETAIL_FILES}>
      <thead>
        <tr><th>{t.backups.file}</th><th>{t.common.size}</th><th></th></tr>
      </thead>
      <tbody>
        {#each snap.detail.files as file}
          <tr>
            <td>{file.path}</td>
            <td class="num">{byteSize(file.size_bytes)}</td>
            <td>
              <!-- The download affordance is a REGULAR-FILE gesture: a directory
                   row gets no dead button (tui's ruling, inherited). -->
              {#if file.file_type === 'regular'}
                <button
                  class="btn"
                  data-testid={IDS.SNAPSHOT_FILE_DOWNLOAD_BUTTON}
                  onclick={() => downloadSnapshotFile(file)}
                  disabled={downloadingPath === file.path}
                >{t.backups.download}</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else if snap?.selected_folder}
    {#if snap.snapshots.length === 0}
      <p class="muted">{t.backups.no_snapshots}</p>
    {:else}
      <table>
        <thead>
          <tr><th>{t.backups.date}</th><th></th></tr>
        </thead>
        <tbody>
          <!-- Wire order (newest-first) — the page does not re-sort. -->
          {#each snap.snapshots as row}
            <tr data-testid={IDS.SNAPSHOT_ITEM} class="clickable" onclick={() => machine?.openSnapshot(row.id)}>
              <!-- ONE cell: the row's whole visible contract (formatted time,
                   file count, formatted size) plus its lifecycle state and, once
                   a check has run, its integrity verdict — never a raw dump. -->
              <td>
                {snapshotRowText(row)}
              </td>
              <td class="row-actions">
                <!-- Recovery's PRESENCE is the row-state observable — rendered
                     ONLY on a `SoftDeleted` row (backups.md § *Soft-deleted
                     rows*), the same shape as `snapshot-prune-execute-button`. -->
                {#if snapshotLifecycle(row.state).kind === 'soft_deleted'}
                  <button
                    data-testid={IDS.SNAPSHOT_UNDELETE_BUTTON}
                    class="btn"
                    disabled={busy}
                    onclick={(e) => { e.stopPropagation(); handleUndeleteSnapshot(row.id); }}
                  >{t.backups.snapshot_undelete_button}</button>
                {/if}
                <button
                  data-testid={IDS.SNAPSHOT_DELETE_BUTTON}
                  class="btn danger"
                  disabled={busy}
                  onclick={(e) => { e.stopPropagation(); handleDeleteSnapshot(row.id); }}
                >{t.common.delete}</button>
                <!-- Immediate-delete: NEVER one-click — opens the friction-bar
                     modal (backups.md rule 4). data-snapshot-id lets the e2e read
                     the row's id (get_attr, scoped to the row) to drive the
                     re-type confirm; the visible text is the date/size, not the
                     raw id. A cross-app contract, not a test nicety. -->
                <button
                  data-testid={IDS.SNAPSHOT_IMMEDIATE_DELETE_BUTTON}
                  data-snapshot-id={row.id}
                  class="btn danger"
                  disabled={busy}
                  onclick={(e) => { e.stopPropagation(); openImmediateDelete(row); }}
                >{t.backups.immediate_delete_button}</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}
{/if}
</div>

{#if ready && $identity}
  <!-- Local restore action (docs/goal/ui/backups.md § Restore from backup
       destination — local path). The backup-destination picker
       (`restore-source-select`) is disabled-at-zero-destinations and not
       wireable yet; the local-snapshot path is the implemented case. -->
  <section class="restore-action">
    <h2>{t.backups.restore_local_title}</h2>

    <select
      data-testid={IDS.RESTORE_SNAPSHOT_SELECT}
      bind:value={selectedRestoreId}
      disabled={restoreSnapshots.length === 0}
    >
      {#each restoreSnapshots as snap}
        <option value={String(snap.id)}>{snapshotRestoreOptionLabel(snap.message_kind, snap.id)}</option>
      {/each}
    </select>

    <div class="kinds" data-testid={IDS.RESTORE_KINDS_CHECKBOXES}>
      <label>
        <input type="checkbox" data-testid={IDS.RESTORE_KIND_CHECKBOX} bind:checked={restoreKindMail} />
        {t.backups.restore_kinds_mail}
      </label>
      <label>
        <input
          type="checkbox"
          data-testid={IDS.RESTORE_KIND_CHECKBOX}
          bind:checked={restoreKindCalendar}
        />
        {t.backups.restore_kinds_calendar}
      </label>
    </div>

    <input
      type="text"
      data-testid={IDS.RESTORE_CONFIRM_INPUT}
      placeholder={t.backups.restore_confirm_placeholder}
      bind:value={restoreConfirm}
    />

    <button
      data-testid={IDS.RESTORE_CONFIRM_BUTTON}
      class="btn"
      onclick={triggerRestore}
      disabled={!restoreButtonEnabled}
    >{t.backups.restore_confirm_button}</button>

    <p class="muted" data-testid={IDS.RESTORE_PROGRESS}>{restoreProgress}</p>
    {#if restoreConfigAbsent}
      <!-- Completed-with-caveat, never error-message: the restore succeeded. -->
      <p class="warn" data-testid={IDS.RESTORE_WARNING}>{t.backups.restore_warning_config_absent}</p>
    {/if}
  </section>

  <!-- Restore history (docs/goal/ui/backups.md § Restore history). Each row
       carries a per-row divergence banner (rendered only when ≥1 divergence
       row exists) that opens the forensic details modal. -->
  <section data-testid={IDS.RESTORE_HISTORY_SECTION} class="restore-history">
    <h2>{t.backups.restore_section_title}</h2>
    <div data-testid={IDS.RESTORE_HISTORY_LIST}>
      {#each restoreHistory as row}
        <div data-testid={IDS.RESTORE_HISTORY_ITEM} class="restore-history-item">
          <span>{restoreHistoryRowText(row)}</span>
          {#if (divergence[row.snapshot_id]?.length ?? 0) > 0}
            <button
              data-testid={IDS.RESTORE_DIVERGENCE_BANNER}
              class="banner"
              onclick={() => openDivergenceModal(row.snapshot_id)}
            >{t.backups.restore_divergence_banner({
                count: String(divergence[row.snapshot_id].length),
              })}</button>
          {/if}
        </div>
      {/each}
    </div>
  </section>

  <!-- Manage backup destinations (docs/goal/ui/backups.md § Manage backup
       destinations). The cross-location destination list — add / edit / remove
       a nest the owner administers. The web twin of the Linux
       views/backups/destinations.rs glue: add resolves identity + verifies
       reachability then records via fauna.account.state.put; edit renames / re-points
       (same-nest only); remove is a plain config edit (the coordinator
       reconciles the offsite deregistration). The per-row status numbers come
       from the NEST's fauna.backup.status projection (backups.md
       § Per-destination status read) via the wasm backupDestinationStatus
       export — the uniform 7-client read since the slice-4 leg (d) repoint
       (2026-07-24). last_upload is a real timestamp here now: the nest's own
       coordinator is what uploads for a web-only user, so the page no longer
       reads "never" forever. -->

  <section class="backup-destinations">
    <h2>{t.backups.backup_destinations_title}</h2>
    <p class="muted">{t.backups.backup_destinations_desc}</p>

    <button
      data-testid={IDS.BACKUP_DESTINATION_ADD_BUTTON}
      class="btn"
      onclick={openAddDest}
    >{t.backups.backup_destination_add_button}</button>

    {#if showDestForm}
      <!-- Inline add/edit dialog (the same form serves both — editingDestId
           distinguishes them). -->
      <div data-testid={IDS.BACKUP_DESTINATION_ADD_MODAL} class="dest-form">
        <h3>
          {editingDestId
            ? t.backups.backup_destination_form_edit_title
            : t.backups.backup_destination_form_add_title}
        </h3>
        <input
          type="text"
          data-testid={IDS.BACKUP_DESTINATION_URL_INPUT}
          placeholder={t.backups.backup_destination_url_placeholder}
          bind:value={destUrl}
        />
        <input
          type="text"
          data-testid={IDS.BACKUP_DESTINATION_NAME_INPUT}
          placeholder={t.backups.backup_destination_name_placeholder}
          bind:value={destName}
        />
        {#if destBusy}
          <p class="muted">{t.backups.backup_destination_resolving}</p>
        {/if}
        <div class="dest-form-actions">
          <button
            data-testid={IDS.BACKUP_DESTINATION_ADD_CANCEL_BUTTON}
            class="btn"
            onclick={cancelDest}
            disabled={destBusy}
          >{t.backups.backup_destination_add_cancel}</button>
          <button
            data-testid={IDS.BACKUP_DESTINATION_ADD_CONFIRM_BUTTON}
            class="btn primary"
            onclick={submitDest}
            disabled={destBusy || destUrl.trim() === ''}
          >{t.backups.backup_destination_add_confirm}</button>
        </div>
      </div>
    {/if}

    <!-- The sole-client durability warning, above the rows it is about and
         painted only while true (the `backup-audit-alert` idiom, and linux's
         placement). Deliberately NOT an alert: nothing is failing — the
         durability story is just weaker than a user may assume
         (backups.md § Third destination kind → Durability + labeling). -->
    {#if soleClientDestinations}
      <p
        class="banner sole-client-warning"
        data-testid={IDS.BACKUP_SOLE_CLIENT_DESTINATION_WARNING}
      >{t.backups.backup_sole_client_destination_warning}</p>
    {/if}

    {#if destinations.length === 0 && destinationsHydrated}
      <!-- ONLY once a read actually succeeded. Before that the list is unknown,
           and the error above is what says so — an unconditional placeholder
           here is the same lie the swallowed catch used to tell. -->
      <p class="muted">{t.backups.backup_destinations_empty}</p>
    {:else}
      {#each destinations as d (d.destination_id)}
        <div data-testid={IDS.BACKUP_DESTINATION_STATUS_ROW} class="dest-row">
          <div class="dest-info">
            <span class="dest-name">{destLabel(d)}</span>
            <!-- kind-badge — the visible half of "a client custodian never
                 silently satisfies *you have an off-site backup*". -->
            <span
              class="muted caption"
              data-testid={IDS.BACKUP_DESTINATION_KIND_BADGE}
            >{kindBadgeText(d)}</span>
            <!-- usage — client-device rows only (ui.yaml). A nest row has no cap
                 and no held-bytes report, so the element is absent rather than
                 empty. -->
            {#if isClientDevice(d)}
              <span
                class="muted caption"
                data-testid={IDS.BACKUP_DESTINATION_USAGE}
              >{usageText(d)}</span>
            {/if}
            <span
              class="muted caption"
              data-testid={IDS.BACKUP_DESTINATION_LAST_UPLOAD_TIME}
            >{lastUploadText(d.destination_id)}</span>
            <span
              class="muted caption"
              data-testid={IDS.BACKUP_DESTINATION_BACKLOG_COUNT}
            >{backlogText(d.destination_id)}</span>
            <!-- The client's own independent check for a nest row, "never" until
                 one passes (backups.md § Audit-alert surface). A healthy page
                 thereby shows positive evidence the audit runs, so the
                 no-device-awake case is visible before it becomes overdue. A
                 client-device row carries its own self-audit instead — dispatched
                 by kind, never re-derived. -->
            <span
              class="muted caption"
              data-testid={IDS.BACKUP_DESTINATION_LAST_AUDIT_TIME}
            >{isClientDevice(d) ? selfAuditText(d.destination_id) : lastAuditText(d.destination_id)}</span>
          </div>
          <button
            data-testid={IDS.BACKUP_DESTINATION_EDIT_BUTTON}
            class="btn"
            onclick={() => openEditDest(d)}
          >{t.backups.backup_destination_edit_button}</button>
          <button
            data-testid={IDS.BACKUP_DESTINATION_REMOVE_BUTTON}
            class="btn danger"
            onclick={() => armRemoveDest(d.destination_id)}
          >{t.backups.backup_destination_remove_button}</button>
          <!-- The post-succession review mark and its Keep half — present ONLY
               while this row is actually raised (succession-aftermath.md
               § Adjudicating what the aftermath carries across). Three shape
               rules taken from tui rather than re-derived, so the four apps
               still owed this pair inherit the reasoning:

               1. ABSENT, not empty, on an ordinary row. In a healthy account
                  every destination is the owner's own, so a permanently
                  rendered element would train the user straight past the one
                  succession that matters.
               2. Remove is deliberately NOT re-rendered here — the row already
                  carries `backup-destination-remove-button` above, so Keep
                  joins the affordance that exists rather than minting a second
                  removal path.
               3. Keep gets NO confirm dialog, unlike Remove: it is
                  non-destructive and re-decidable (the row stays removable
                  forever after), so the friction bar Remove carries would be
                  friction with no payoff. -->
          {#if d.unattested}
            <span
              class="muted caption dest-unattested"
              data-testid={IDS.BACKUP_DESTINATION_UNATTESTED_MARK}
            >{t.backups.backup_destination_unattested_mark}</span>
            <button
              data-testid={IDS.BACKUP_DESTINATION_KEEP_BUTTON}
              class="btn"
              disabled={destBusy}
              onclick={() => keepDest(d.destination_id)}
            >{t.backups.backup_destination_keep_button}</button>
          {/if}
        </div>
      {/each}
    {/if}

    {#if removingDestId !== null}
      <!-- Inline remove-confirm dialog (warns the offsite copy is reclaimed). -->
      <div
        data-testid={IDS.BACKUP_DESTINATION_REMOVE_CONFIRM_MODAL}
        class="dest-remove-confirm"
      >
        <p>{t.backups.backup_destination_remove_confirm_title}</p>
        <div class="dest-form-actions">
          <button
            data-testid={IDS.BACKUP_DESTINATION_REMOVE_CANCEL_BUTTON}
            class="btn"
            onclick={cancelRemoveDest}
            disabled={destBusy}
          >{t.backups.backup_destination_remove_cancel_button}</button>
          <button
            data-testid={IDS.BACKUP_DESTINATION_REMOVE_CONFIRM_BUTTON}
            class="btn danger"
            onclick={confirmRemoveDest}
            use:offlineGate={{ kind: 'fauna.backup.destination.remove', disabled: destBusy }}
          >{t.backups.backup_destination_remove_confirm_button}</button>
        </div>
      </div>
    {/if}
  </section>
{/if}

{#if divergenceModalRows !== null}
  <!-- Forensic divergence-details modal — read-only, close only (no action
       buttons; server state already won). -->
  <div class="modal-backdrop" role="presentation" onclick={() => (divergenceModalRows = null)}>
    <div
      data-testid={IDS.RESTORE_DIVERGENCE_DETAILS_MODAL}
      class="modal"
      role="dialog"
      aria-modal="true"
      onclick={(e) => e.stopPropagation()}
    >
      <h2>{t.backups.restore_divergence_modal_title}</h2>
      {#each divergenceModalRows as row}
        <p data-testid={IDS.RESTORE_DIVERGENCE_DETAILS_ITEM}>{divergenceDetailText(row)}</p>
      {/each}
      <p class="muted footer">{t.backups.restore_divergence_footer}</p>
      <button class="btn" onclick={() => (divergenceModalRows = null)}
        >{t.backups.restore_divergence_close}</button
      >
    </div>
  </div>
{/if}

{#if immediateDeleteTargetId !== null}
  <!-- Immediate-delete confirmation modal (docs/goal/ui/backups.md § Element IDs
       + rule 4). The friction bar: confirm enables ONLY when the user re-types
       BOTH the snapshot id AND the exact acknowledge phrase (shown below). On
       confirm → the machine's delete_immediate; the modal is closed by the RENDER
       PASS once the row leaves the list, so the nest's hard-floor rejection leaves
       it standing with the error on `error-message`. The backdrop click cancels
       (no side effect), matching the cancel button. -->
  <div class="modal-backdrop" role="presentation" onclick={cancelImmediateDelete}>
    <div
      data-testid={IDS.IMMEDIATE_DELETE_CONFIRM_MODAL}
      class="modal"
      role="dialog"
      aria-modal="true"
      onclick={(e) => e.stopPropagation()}
    >
      <h2>{t.backups.immediate_delete_modal_title({ id: String(immediateDeleteTargetId) })}</h2>
      <p class="warn">{t.backups.immediate_delete_warning}</p>

      <input
        type="text"
        data-testid={IDS.IMMEDIATE_DELETE_CONFIRM_INPUT}
        placeholder={t.backups.immediate_delete_confirm_id_placeholder}
        bind:value={immediateDeleteConfirmId}
      />

      <p class="ack-prompt">{t.backups.immediate_delete_acknowledge_prompt}</p>
      <p class="ack-phrase">{immediateDeleteAck}</p>
      <input
        type="text"
        data-testid={IDS.IMMEDIATE_DELETE_ACKNOWLEDGE_INPUT}
        placeholder={t.backups.immediate_delete_acknowledge_placeholder}
        bind:value={immediateDeleteAcknowledge}
      />

      <div class="modal-actions">
        <button
          data-testid={IDS.IMMEDIATE_DELETE_CANCEL_BUTTON}
          class="btn"
          onclick={cancelImmediateDelete}
          disabled={busy}
        >{t.backups.immediate_delete_cancel_button}</button>
        <button
          data-testid={IDS.IMMEDIATE_DELETE_CONFIRM_BUTTON}
          class="btn danger"
          onclick={confirmImmediateDelete}
          disabled={!immediateDeleteButtonEnabled}
        >{t.backups.immediate_delete_confirm_button}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .folder-row { display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.5rem; }
  .last-backed-up { color: var(--text-muted); font-size: 0.8125rem; margin-bottom: 0.5rem; }
  .prune-preview {
    margin: 0.75rem 0;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .prune-preview h3 { font-size: 1rem; margin: 0 0 0.5rem; }
  .prune-preview-actions { display: flex; gap: 0.5rem; margin-top: 0.5rem; }
  .check-result { margin-bottom: 0.5rem; }
  .warn { color: var(--danger, #c0392b); }
  .muted { color: var(--text-muted); }
  .back {
    background: none;
    border: none;
    color: var(--link);
    cursor: pointer;
    padding: 0;
    margin-bottom: 1rem;
    font-size: 0.875rem;
  }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid var(--border); }
  .num { text-align: right; }
  .clickable { cursor: pointer; }
  .clickable:hover { background: var(--hover); }

  .restore-action, .restore-history, .backup-destinations { margin-top: 2rem; }
  .restore-action h2, .restore-history h2, .backup-destinations h2 {
    font-size: 1.125rem;
    margin-bottom: 0.5rem;
  }
  .backup-destinations h3 { font-size: 1rem; margin: 0.5rem 0; }
  .caption { font-size: 0.8125rem; }
  .dest-form, .dest-remove-confirm {
    margin: 0.75rem 0;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .dest-form input[type="text"] { display: block; margin-bottom: 0.5rem; min-width: 18rem; }
  .dest-form-actions { display: flex; gap: 0.5rem; }
  .dest-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .dest-info { display: flex; flex-direction: column; gap: 0.125rem; flex: 1; }
  .dest-name { font-weight: 600; }
  .btn.danger { color: var(--danger, #c0392b); }
  .btn.primary { font-weight: 600; }
  .restore-action select,
  .restore-action input[type="text"] { display: block; margin-bottom: 0.5rem; min-width: 16rem; }
  .kinds { display: flex; gap: 1rem; margin-bottom: 0.5rem; }
  .kinds label { display: inline-flex; align-items: center; gap: 0.25rem; }
  .restore-history-item {
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    align-items: flex-start;
  }
  .banner {
    background: none;
    border: 1px solid var(--warning, #b58900);
    color: var(--warning, #b58900);
    border-radius: 4px;
    padding: 0.125rem 0.5rem;
    cursor: pointer;
    font-size: 0.8125rem;
  }
  /* The audit banner is a `<p>`, not the clickable `restore-divergence-banner`
     `<button>` this class was written for — it opens nothing, it states a finding.
     Full width so it reads as a page-level warning rather than an inline chip. */
  .audit-alert {
    cursor: default;
    display: block;
    margin: 0.25rem 0;
    padding: 0.375rem 0.625rem;
  }
  /* The sole-client warning shares the audit banner's page-level shape (a
     stating `<p>`, not the clickable chip `.banner` was written for), but it is
     not an alert — nothing is failing, so it stays the calmer warning colour
     `.banner` already carries rather than borrowing the error palette. */
  .sole-client-warning {
    cursor: default;
    display: block;
    margin: 0.25rem 0;
    padding: 0.375rem 0.625rem;
  }
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .modal {
    background: var(--bg, #fff);
    padding: 1.5rem;
    border-radius: 8px;
    max-width: 560px;
    max-height: 80vh;
    overflow-y: auto;
  }
  .modal .footer { margin-top: 1rem; font-size: 0.8125rem; }
  .modal .warn { color: var(--danger, #c0392b); margin-bottom: 1rem; }
  .modal input[type="text"] { display: block; width: 100%; margin-bottom: 0.5rem; }
  .modal .ack-prompt { margin: 0.75rem 0 0.25rem; }
  .modal .ack-phrase {
    font-family: monospace;
    font-weight: 600;
    margin-bottom: 0.5rem;
    user-select: all;
  }
  .modal .modal-actions { display: flex; gap: 0.5rem; justify-content: flex-end; margin-top: 1rem; }
  .row-actions { display: flex; gap: 0.5rem; }
</style>
