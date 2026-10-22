// The web identity store's unlocked key — held in page memory and nowhere
// else (`docs/goal/architecture/apps/web.md` § Identity store at rest → *The
// unlocked store key lives in page memory and nowhere else*).
//
// Every wasm chunk that links `fauna-client-accounts` is a separate runtime
// with its own locked `SealedSecretStore`, so the page installs the key the
// launch chunk unlocked into each one as it loads (`registerIdentityStoreChunk`,
// called by every chunk loader right after its init) and into every chunk
// already loaded the moment it is unlocked (`adoptStoreKey`). Each install is
// checked Rust-side against the seal header, so a stale or foreign key is
// refused and never seals a write.
//
// Between tabs the key travels over a same-origin `BroadcastChannel`: a newly
// opened tab asks, an unlocked tab answers, so one unlock serves the browser
// session for as long as one tab stays open. The key is never written to
// `localStorage`, IndexedDB or `sessionStorage`.

/** The doors every identity-store-carrying chunk exports
 *  (`libs/fauna-client-accounts/src/web_store.rs`). */
export interface IdentityStoreDoors {
  identityStoreHasHeader(): boolean;
  identityStoreIsLocked(): boolean;
  identityStoreKey(): string | undefined;
  identityStoreInstallKey(keyHex: string): void;
  identityStoreLock(): void;
}

function hasDoors(mod: unknown): mod is IdentityStoreDoors {
  return (
    typeof mod === 'object' &&
    mod !== null &&
    typeof (mod as Partial<IdentityStoreDoors>).identityStoreInstallKey === 'function'
  );
}

const chunks: IdentityStoreDoors[] = [];
let storeKeyHex: string | null = null;

function install(chunk: IdentityStoreDoors, keyHex: string): void {
  try {
    chunk.identityStoreInstallKey(keyHex);
  } catch (code) {
    // A refused key (`wrong_store_key` — the store was started over meanwhile)
    // leaves the chunk locked: it reads nothing and writes nothing, which is
    // the safe side. The unlock gate is the recovery.
    console.warn('identity store: a chunk refused the store key:', code);
  }
}

/** Register a freshly initialized wasm chunk. A chunk without the doors (one
 *  that never touches the identity store) is ignored. Idempotent per module. */
export function registerIdentityStoreChunk(mod: unknown): void {
  if (!hasDoors(mod) || chunks.includes(mod)) return;
  chunks.push(mod);
  if (storeKeyHex) install(mod, storeKeyHex);
}

/** Whether this page holds the unlocked store key. */
export function hasStoreKey(): boolean {
  return storeKeyHex !== null;
}

/** The page now holds `keyHex` (unlocked here, or handed by another tab and
 *  already confirmed by the launch chunk): install it into every loaded chunk
 *  and answer other tabs that ask for it. */
export function adoptStoreKey(keyHex: string): void {
  storeKeyHex = keyHex;
  for (const chunk of chunks) {
    if (chunk.identityStoreIsLocked()) install(chunk, keyHex);
  }
  ensureChannel();
}

/** Forget the key in this page and lock every chunk (start-over). */
export function forgetStoreKey(): void {
  storeKeyHex = null;
  for (const chunk of chunks) chunk.identityStoreLock();
}

// ── The tab hand-off ────────────────────────────────────────────────────────

const CHANNEL_NAME = 'fauna-identity-store';

type HandOff =
  | { kind: 'key-request' }
  | { kind: 'key'; key: string }
  | { kind: 'started-over' };

let channel: BroadcastChannel | null = null;
const keyWaiters = new Set<(keyHex: string) => void>();

function ensureChannel(): BroadcastChannel | null {
  if (channel) return channel;
  if (typeof BroadcastChannel === 'undefined') return null;
  channel = new BroadcastChannel(CHANNEL_NAME);
  channel.onmessage = (ev: MessageEvent<HandOff>) => {
    const msg = ev.data;
    if (!msg || typeof msg !== 'object') return;
    if (msg.kind === 'key-request') {
      if (storeKeyHex) channel?.postMessage({ kind: 'key', key: storeKeyHex } satisfies HandOff);
    } else if (msg.kind === 'key') {
      if (!storeKeyHex && typeof msg.key === 'string') {
        for (const waiter of [...keyWaiters]) waiter(msg.key);
      }
    } else if (msg.kind === 'started-over') {
      // Another tab erased this browser's identity store: the key this page
      // holds no longer opens anything. Back to the gate (create mode).
      if (storeKeyHex) {
        forgetStoreKey();
        location.reload();
      }
    }
  };
  return channel;
}

/** Ask the tabs already unlocked for the store key. `onKey` receives every
 *  answer until the returned stop function is called; the caller confirms a
 *  key (the launch chunk's `identityStoreInstallKey`) before adopting it. */
export function requestStoreKeyFromTabs(onKey: (keyHex: string) => void): () => void {
  const ch = ensureChannel();
  if (!ch) return () => {};
  keyWaiters.add(onKey);
  ch.postMessage({ kind: 'key-request' } satisfies HandOff);
  return () => keyWaiters.delete(onKey);
}

/** Tell the other tabs this browser's identity store was started over. */
export function announceStartOver(): void {
  ensureChannel()?.postMessage({ kind: 'started-over' } satisfies HandOff);
}
