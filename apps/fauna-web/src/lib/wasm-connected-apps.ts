import type { SharedRpcPort } from '../../static/fauna_wasm_connected_apps.js';
import { base } from '$app/paths';
import type { ConnectedAppsMachine } from '../../static/fauna_wasm_connected_apps.js';
import { hexToBytes } from './hex';
import { sharedAccountPort } from './account-runtime';
import { nodeUrl } from './api';
import { localTimestamp } from './value-format';

// Re-export so route/component files consume the machine type via
// `$lib/wasm-connected-apps` rather than a fragile relative path into
// `static/` (mirrors `wasm-labeler-catalog.ts`).
export type { ConnectedAppsMachine };

// Loader for the page-level connected-apps WASM chunk
// (`libs/fauna-wasm-connected-apps`: the `ConnectedAppsMachine` — the roster of
// everything acting for the user from outside the apps, the Requests tray,
// Connect an app, per-client blocks). A separate wasm module from `fauna_wasm`
// — same separate-chunk discipline: keep the constructor call inside the module
// that holds the singleton `wasmModule` reference (a different Vite chunk would
// carry its own copy of the wasm-bindgen boilerplate). One shared surface, all
// 7 apps (priority #1/#2; docs/goal/ui/connected-apps.md).

let wasmModule: typeof import('../../static/fauna_wasm_connected_apps.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the connected-apps wasm chunk exactly once per page load. The
 *  init *promise* is memoized (same load-bearing shape as
 *  `wasm.ts::ensureWasm`): two concurrent callers await the SAME in-flight
 *  init, so `mod.default()` runs once — a second concurrent call would
 *  re-instantiate the chunk and reset wasm linear memory. A failed init
 *  drops the cached promise so a later call can retry. */
export function ensureConnectedAppsWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_connected_apps.js');
      await mod.default(`${base}/fauna_wasm_connected_apps_bg.wasm`);
      wasmModule = mod;
    })().catch((e) => {
      wasmInit = null;
      throw e;
    });
  }
  return wasmInit;
}

function wasm() {
  if (!wasmModule) {
    throw new Error('Connected-apps WASM not initialized — call ensureConnectedAppsWasm() first');
  }
  return wasmModule;
}

/** Observer the page registers with the machine; `onChanged` fires on every
 *  state tick (refresh, code lookup, resolve, block, revoke). */
export interface ConnectedAppsMachineObserver {
  onChanged(): void;
}

/**
 * Build the page-level `ConnectedAppsMachine` over the SPA singleton's socket —
 * `port` is `sharedRpcPort(secretHex)` from `$lib/rpc`. `secretHex` is the
 * actor's 32-byte seed: the chunk builds its own mail-settings machine for the
 * mail app-password rows (the one Mail & Calendar builds, with the same
 * `nodeUrl()`), reading the account's mail custody and succession ledger
 * through this tab's account port, `sharedAccountPort(secretHex)`. State starts
 * empty — the caller drives `refresh()`, which hydrates that mail machine first.
 */
export async function createConnectedAppsMachine(
  observer: ConnectedAppsMachineObserver,
  port: SharedRpcPort,
  secretHex: string,
): Promise<ConnectedAppsMachine> {
  await ensureConnectedAppsWasm();
  return new (wasm().ConnectedAppsMachine)(
    observer,
    port,
    hexToBytes(secretHex),
    sharedAccountPort(secretHex),
    nodeUrl(),
  );
}

/** A roster row's text as composed once in shared Rust (`compose::roster_row_text`):
 *  `lines` top to bottom, then the `facts` the page joins with `factsSeparator()`
 *  as the last line. Resolve each with `resolveComposed`. Instants are formatted
 *  in JS (`localTimestamp`) — wasm has no OS timezone database. */
export interface RosterRowText {
  lines: ComposedText[];
  facts: ComposedText[];
}

/** Compose one snapshot row's text. Needs the chunk initialized (it is: a row
 *  only exists after the machine was built). */
export function composeRosterRow(row: ConnectedAppRow): RosterRowText {
  return JSON.parse(wasm().rosterRowTextJson(JSON.stringify(row), localTimestamp)) as RosterRowText;
}

/** Compose one request card's lines (`compose::request_card_lines`). */
export function composeRequestCard(card: ConsentCardRow): ComposedText[] {
  return JSON.parse(wasm().requestCardLinesJson(JSON.stringify(card))) as ComposedText[];
}

/** What a roster row's facts line puts between its facts. */
export function factsSeparator(): string {
  return wasm().factsSeparator();
}

// ── Snapshot shapes — `ConnectedAppsSnapshot` (libs/fauna-client-connected-apps/
// src/snapshots.rs) as `snapshotJson()` serializes it. The web page renders off
// these verbatim: roster composition, scope words, the class key and which verb
// revokes a row are all the shared machine's.
import type { ComposedText, LocalizedText } from '$lib/i18n/localized';
import type { ConsentCardRow, ConsentSetRow } from '$lib/atproto-settings-machine';

export type { ConsentCardRow };

/** One roster row. `key` is opaque — the machine picks the revoke verb from it. */
export interface ConnectedAppRow {
  key: string;
  /** A `class::*` value (`remote`, `device`, `wasm`, `container`, `app_password`,
   *  `signer`, `oauth`) or an unknown execution form — no badge then. */
  class: string;
  name: LocalizedText;
  client_id: string | null;
  publisher: string | null;
  scope_descriptions: LocalizedText[];
  /** The permission sets the row's permissions came from, in the consent
   *  card's own composition; empty for every non-principal row. */
  sets: ConsentSetRow[];
  created_at_millis: number;
  last_used_at_millis: number | null;
  lasts_until_millis: number | null;
  connected: boolean;
  /** The account's external-apps switch is OFF and this row is on the plane it
   *  pauses (a principal or an ATProto app-password session): listed and
   *  revocable, not connected, works again once the switch is back ON. */
  paused: boolean;
  /** Set on a mail app-password row only: the MUA login (only `{handle}` left
   *  for the renderer — `resolveMuaUsername`), the kind label, and whether the
   *  identity-succession burn killed it. */
  mail: { mua_username: string; kind: LocalizedText; revoked: boolean } | null;
  /** A NIP-46 signer row the identity-succession burn killed and kept: listed
   *  not connected, the mail row's `revoked` for the signer class. */
  signer_burned: boolean;
  /** The oracle summary — present exactly on a principal the nest's custodian
   *  served, or an ATProto app-password session that has written, never
   *  zero-filled on one it did not. */
  oracle?: OracleSummaryRow | null;
}

/** What the oracle did for one principal: operations in the current window,
 *  every request answered, and the newest operation (`snapshots.rs`). */
export interface OracleSummaryRow {
  ops_in_window: number;
  requests_served: number;
  last_op: OracleOpRow | null;
}

export interface OracleOpRow {
  at_millis: number;
  class: string;
  words: LocalizedText;
  detail: LocalizedText;
}

export interface BlockedAppRow {
  client_id: string;
  blocked_at_millis: number;
}

export interface ConnectedAppsSnapshot {
  loaded: boolean;
  requests: ConsentCardRow[];
  principals: ConnectedAppRow[];
  blocked: BlockedAppRow[];
  error: LocalizedText | null;
}
