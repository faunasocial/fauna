import type { SharedRpcPort } from '../../static/fauna_wasm_connected_apps.js';
import { base } from '$app/paths';
import type { ConnectedAppsMachine } from '../../static/fauna_wasm_connected_apps.js';

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
 * `port` is `sharedRpcPort(secretHex)` from `$lib/rpc`. State starts empty —
 * the caller drives `refresh()`.
 */
export async function createConnectedAppsMachine(
  observer: ConnectedAppsMachineObserver,
  port: SharedRpcPort,
): Promise<ConnectedAppsMachine> {
  await ensureConnectedAppsWasm();
  return new (wasm().ConnectedAppsMachine)(observer, port);
}

// ── Snapshot shapes — `ConnectedAppsSnapshot` (libs/fauna-client-connected-apps/
// src/snapshots.rs) as `snapshotJson()` serializes it. The web page renders off
// these verbatim: roster composition, scope words, the class key and which verb
// revokes a row are all the shared machine's.
import type { LocalizedText } from '$lib/i18n/localized';
import type { ConsentCardRow } from '$lib/atproto-settings-machine';

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
  created_at_millis: number;
  last_used_at_millis: number | null;
  lasts_until_millis: number | null;
  connected: boolean;
  /** `Some` on a mail app-password row only. Web's machine is built without the
   *  mail machine, so it is always null here until the mail rows reach web. */
  mail: { mua_username: string; kind: LocalizedText; revoked: boolean } | null;
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
