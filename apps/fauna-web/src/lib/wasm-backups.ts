import type { SharedRpcPort } from '../../static/fauna_wasm_backups.js';
import { base } from '$app/paths';
import { sharedAccountPort } from './account-runtime';
import type { BackupsMachine } from '../../static/fauna_wasm_backups.js';
import type { LocalizedText } from '$lib/i18n/localized';
import type { BackupOp, RowIntegrity, SnapshotState } from '$lib/backups-machine';

// Loader for the Backups-page WASM chunk (`libs/fauna-wasm-backups`: the
// snapshot half's `BackupsMachine`). A separate wasm module from `fauna_wasm` —
// same separate-chunk discipline as `wasm-folders.ts` / `wasm-media.ts`: keep
// the constructor call inside the module that holds the singleton `wasmModule`
// reference (a different Vite chunk would carry its own copy of the
// wasm-bindgen boilerplate, and constructing the class from elsewhere would hit
// uninitialized memory).
//
// The Backups page builds one `BackupsMachine` over its own short-lived WS-RPC
// connection (the standalone constructor), exactly as the Devices page builds
// its `DevicesMachine`.

let wasmModule: typeof import('../../static/fauna_wasm_backups.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the backups wasm chunk exactly once per page load. The init
 *  *promise* is memoized (same load-bearing shape as `wasm.ts::ensureWasm`):
 *  two concurrent callers await the SAME in-flight init, so `mod.default()`
 *  runs once — a second concurrent call would re-instantiate the chunk and
 *  reset wasm linear memory. A failed init drops the cached promise so a
 *  later call can retry. */
export function ensureBackupsWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_backups.js');
      await mod.default(`${base}/fauna_wasm_backups_bg.wasm`);
      wasmModule = mod;
    })().catch((e) => {
      wasmInit = null;
      throw e;
    });
  }
  return wasmInit;
}

function wasm() {
  if (!wasmModule) throw new Error('Backups WASM not initialized — call ensureBackupsWasm() first');
  return wasmModule;
}

/** Observer the page registers with the machine; `onChanged` fires on every
 *  state tick (refresh, gesture, or a completed round trip). */
export interface BackupsMachineObserver {
  onChanged(): void;
}

/**
 * Build the page-level `BackupsMachine` over the SPA singleton's socket —
 * `port` is `sharedRpcPort(secretHex)` from `$lib/rpc`. State starts empty —
 * the caller drives `refresh()`.
 *
 * `ownerSecretHex` is the identity secret the machine derives its owner
 * `BackupKey` from, and it is **not optional in practice**: without it a sealed
 * set's `snapshot-detail-files` renders empty for a reader who holds the key — the same reason the `snapshotGet` binding this machine
 * replaces took one. Pass `$identity.secretHex`.
 */
export async function createBackupsMachine(
  observer: BackupsMachineObserver,
  port: SharedRpcPort,
  ownerSecretHex: string,
): Promise<BackupsMachine> {
  await ensureBackupsWasm();
  return new (wasm().BackupsMachine)(
    observer,
    port,
    ownerSecretHex,
    // The account's folder-key custody, read through the tab's account runtime.
    sharedAccountPort(ownerSecretHex),
  );
}

/** `fauna_backups_machine::busy_text` → the `in_progress_op` line for a
 *  `BackupOp`, so web renders the same text as every other app without
 *  re-deriving which key an operation maps to
 *  (`docs/goal/ui/backups.md` § Where logic lives). Call after
 *  `ensureBackupsWasm()` has resolved. */
export function busyText(op: BackupOp): LocalizedText {
  return wasm().busyText(op) as LocalizedText;
}

/** `fauna_backups_machine::snapshot_state_text` → the `snapshot-item[i]`
 *  lifecycle suffix for a `SnapshotState`. `formattedDeadline` is web's own
 *  locale-aware rendering of the state's deadline
 *  (`behavior/value-formatting.md` § Relative time) — this face owns only
 *  the rule (which key, and the dated/undated fallback), never timestamp
 *  formatting. `undefined` when the state is `Active`. */
export function snapshotStateText(
  state: SnapshotState,
  formattedDeadline: string | null,
): LocalizedText | undefined {
  return wasm().snapshotStateText(state, formattedDeadline ?? undefined) as
    | LocalizedText
    | undefined;
}

/** `fauna_backups_machine::snapshot_integrity_text` → the `snapshot-item[i]`
 *  integrity suffix for a `RowIntegrity`. `undefined` for `'Unknown'` —
 *  absent until a check runs this session, never the word "unknown"
 *  (`docs/goal/ui/backups.md` § Snapshot-list shape, *Check*). */
export function snapshotIntegrityText(integrity: RowIntegrity): LocalizedText | undefined {
  return wasm().snapshotIntegrityText(integrity) as LocalizedText | undefined;
}
