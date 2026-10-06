import type { SharedRpcPort } from '../../static/fauna_wasm_labeler_catalog.js';
import { base } from '$app/paths';
import type { LabelerCatalogMachine } from '../../static/fauna_wasm_labeler_catalog.js';
import { hexToBytes } from './hex';
import { sharedAccountPort } from './account-runtime';

// Re-export so route/component files consume the machine type via
// `$lib/wasm-labeler-catalog` rather than a fragile relative path into
// `static/` (mirrors `wasm-media.ts` / `wasm-folders.ts`).
export type { LabelerCatalogMachine };

// Loader for the page-level community-labeler-catalog WASM chunk
// (`libs/fauna-wasm-labeler-catalog`: the `LabelerCatalogMachine` — browse /
// inspect-before-subscribe / (un)subscribe). A separate wasm module from
// `fauna_wasm` (and from `fauna_wasm_folders` / `fauna_wasm_media`) — same
// separate-chunk discipline: keep the constructor call inside the module
// that holds the singleton `wasmModule` reference (a different Vite chunk
// would carry its own copy of the wasm-bindgen boilerplate).
//
// The Personalization home + Community-labelers catalog settings sub-pages
// each build their own `LabelerCatalogMachine` over a short-lived WS-RPC
// connection, exactly as the Devices / Media pages build theirs — one shared
// surface, all 7 apps (priority #1/#2;
// docs/goal/architecture/content-moderation-and-ranking.md § Tier-3).

let wasmModule: typeof import('../../static/fauna_wasm_labeler_catalog.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the labeler-catalog wasm chunk exactly once per page load. The
 *  init *promise* is memoized (same load-bearing shape as
 *  `wasm.ts::ensureWasm`): two concurrent callers await the SAME in-flight
 *  init, so `mod.default()` runs once — a second concurrent call would
 *  re-instantiate the chunk and reset wasm linear memory. A failed init
 *  drops the cached promise so a later call can retry. */
export function ensureLabelerCatalogWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_labeler_catalog.js');
      await mod.default(`${base}/fauna_wasm_labeler_catalog_bg.wasm`);
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
    throw new Error('Labeler-catalog WASM not initialized — call ensureLabelerCatalogWasm() first');
  }
  return wasmModule;
}

/** Observer the page registers with the machine; `onChanged` fires on every
 *  state tick (refresh, inspect, subscribe, unsubscribe). */
export interface LabelerCatalogMachineObserver {
  onChanged(): void;
}

/**
 * Build the page-level `LabelerCatalogMachine` over the SPA singleton's
 * socket — `port` is `sharedRpcPort(secretHex)` from `$lib/rpc`; `secretHex`
 * is the actor's 32-byte ed25519 seed, which gives the machine its grant
 * seams (subscribing a `wasm` mail labeler mints the per-labeler grant,
 * unsubscribing revokes it — recorded through the account's succession
 * ledger and minted from its mail custody, both read through this tab's
 * account port, `sharedAccountPort(secretHex)`).
 * State starts empty — the caller drives
 * `refresh()`.
 */
export async function createLabelerCatalogMachine(
  observer: LabelerCatalogMachineObserver,
  port: SharedRpcPort,
  secretHex: string,
): Promise<LabelerCatalogMachine> {
  await ensureLabelerCatalogWasm();
  return new (wasm().LabelerCatalogMachine)(
    observer,
    port,
    hexToBytes(secretHex),
    sharedAccountPort(secretHex),
  );
}
