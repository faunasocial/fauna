import type { SharedRpcPort } from '../../static/fauna_wasm_media.js';
import { base } from '$app/paths';
import { sharedAccountPort } from './account-runtime';
import type { MediaMachine } from '../../static/fauna_wasm_media.js';
import type { LocalizedText } from '$lib/i18n/localized';

// Re-export so route files consume the machine type via `$lib/wasm-media` rather
// than a fragile relative path into `static/` (mirrors how pages reach the machine).
export type { MediaMachine };

// Loader for the page-level Media WASM chunk (`libs/fauna-wasm-media`: the
// cross-set all-media `MediaMachine`). A separate wasm module from `fauna_wasm`
// (and from `fauna_wasm_folders`) — same separate-chunk discipline as
// `wasm-folders.ts` / `wasm-onboarding.ts`: keep the constructor call inside the
// module that holds the singleton `wasmModule` reference (a different Vite chunk
// would carry its own copy of the wasm-bindgen boilerplate and constructing the
// class from elsewhere would hit uninitialized memory).
//
// The Media page builds one `MediaMachine` over its own short-lived WS-RPC
// connection (the standalone constructor), exactly as the Devices page builds a
// `DevicesMachine` via `$lib/wasm-folders` — one shared surface, all 7 apps
// (priority #1/#2; docs/goal/ui/media.md rule 2).

let wasmModule: typeof import('../../static/fauna_wasm_media.js') | null = null;
let wasmInit: Promise<void> | null = null;

/** Initialize the media wasm chunk exactly once per page load. The init
 *  *promise* is memoized (same load-bearing shape as `wasm.ts::ensureWasm`):
 *  two concurrent callers await the SAME in-flight init, so `mod.default()`
 *  runs once — a second concurrent call would re-instantiate the chunk and
 *  reset wasm linear memory. A failed init drops the cached promise so a
 *  later call can retry. */
export function ensureMediaWasm(): Promise<void> {
  if (wasmModule) return Promise.resolve();
  if (!wasmInit) {
    wasmInit = (async () => {
      const mod = await import('../../static/fauna_wasm_media.js');
      await mod.default(`${base}/fauna_wasm_media_bg.wasm`);
      wasmModule = mod;
    })().catch((e) => {
      wasmInit = null;
      throw e;
    });
  }
  return wasmInit;
}

function wasm() {
  if (!wasmModule) throw new Error('Media WASM not initialized — call ensureMediaWasm() first');
  return wasmModule;
}

/** Observer the page registers with the machine; `onChanged` fires on every state
 *  tick (refresh, gesture, or sort/filter/view-toggle change). */
export interface MediaMachineObserver {
  onChanged(): void;
}

/**
 * Build the page-level `MediaMachine` over the SPA singleton's socket —
 * `port` is `sharedRpcPort(secretHex)` from `$lib/rpc`. State starts empty
 * (all-media view, name sort ascending, list mode) — the caller drives
 * `refresh()`.
 *
 * `secretHex` (the identity seed hex) is required because the machine is handed
 * its read-side folder custody at construction and its write-side label
 * custody right after — see the injections below.
 */
export async function createMediaMachine(
  observer: MediaMachineObserver,
  port: SharedRpcPort,
  secretHex: string,
): Promise<MediaMachine> {
  await ensureMediaWasm();
  // READ-side folder custody goes in through the constructor, not a setter:
  // the shared-folder content-key resolver is a construction-time field of
  // the shared machine on every app (tui/linux/the FFI apps pass it to
  // `build_media_machine_with_folder_keys`), and a machine built without it
  // reads every shared set as owner-only — a member would list the owner's
  // files under sealed names and `download` would fail closed on each. The
  // keypair is derived inside wasm; the raw secret never enters JS.
  const machine: MediaMachine = new (wasm().MediaMachine)(
    observer,
    port,
    secretHex,
    // The account's folder-key custody, read through the tab's account runtime.
    sharedAccountPort(secretHex),
  );
  // Write-side label custody for the delete/restore gestures (S8 D2), injected
  // once at construction exactly as tui and linux do at their build sites. Those
  // two gestures take no per-call key (unlike `uploadSelected`), so without this
  // they seal nothing and the nest refuses the record outright
  // (`fauna.sync.path_seal_required`, post-S9-flip). Handing custody at the
  // construction seam — rather than at each call — is file-sync.md § Sealed
  // names & paths' CONSUMER-WIRING RULE, and it makes a keyless machine
  // unrepresentable here. The BackupKey is derived inside wasm; the raw key
  // never enters JS.
  machine.setOwnerBackupKey(secretHex);
  // READ-side custody for a successor, and deliberately a SECOND call rather
  // than a second key on the line above: that one is the delete/restore seal
  // root, and a retired key must never reach a seal. Without this a media
  // corpus the succession re-pointed to this owner stays dark — the fetch
  // succeeds and only the AEAD tag fails, so the Media page lists nothing and
  // reports no error (succession-aftermath.md § Re-key scope — *media*).
  // Takes the seed for the same reason the line above does: the keys are
  // derived from the account registry inside wasm and never enter JS.
  machine.setPredecessorBackupKeys(secretHex);
  // The followed-public-folder browse scopes (`ui/media.md` § Followed public
  // folders) — wired here for the same reason the backup key is: unwired, the
  // snapshot's `followed` is permanently empty, so `media-folder-filter` offers
  // no followed options and the whole scope is unreachable. Built over the SAME
  // source type the Devices page uses, so a browse fetch and the availability
  // probe share one cached verdict rather than racing two. The follows are the
  // account's `fauna.state.follows` rows, read across the one account port.
  machine.setFollowedMediaSource(sharedAccountPort(secretHex));
  // The share-link author (`share-links.md` § Where logic lives): the seed
  // signs the token and derives the filename-seal root inside wasm, and the
  // links point at the nest this machine's socket rides. AFTER the
  // predecessors, whose keys open old list names — tui's and linux's order.
  machine.setShareAuthor(secretHex);
  return machine;
}

/**
 * The `sync-state-badge` label for a `media-item` row — web is a
 * control-plane client (file-sync.md § Per-file sync-status display), so it
 * always renders the `Synced` state. Resolve via `resolveLocalized(...)`.
 * Requires the Media wasm to be ensured first (the page builds a
 * `MediaMachine` on mount before any row renders).
 */
export function syncedStateBadgeLabel(): LocalizedText {
  return wasm().syncedStateBadgeLabel() as LocalizedText;
}

/** The `share-link-expiry-select` option label for a raw value (`1d` / `7d` /
 *  `30d` / `1y`) — the shared `share_link_expiry_label` map. `null` for an
 *  unknown value, which the page paints raw. */
export function shareLinkExpiryLabel(value: string): LocalizedText | null {
  return (wasm().shareLinkExpiryLabel(value) as LocalizedText | undefined) ?? null;
}

/** The `share-link-item-state` label for a row's stable state (`active` /
 *  `expired` / `revoked`) — the shared `share_link_state_label` map. `null`
 *  for an unknown state, painted raw. */
export function shareLinkStateLabel(state: string): LocalizedText | null {
  return (wasm().shareLinkStateLabel(state) as LocalizedText | undefined) ?? null;
}
