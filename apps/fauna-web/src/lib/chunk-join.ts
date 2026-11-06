import { registerIdentityStoreChunk } from './identity-store';
import { logMessage } from './wasm';

/** The doors every page-level wasm chunk exports besides the core one. */
export interface JoinableChunk {
  /** `fauna_wasm_panic_hook::forward_tracing`: this chunk's `tracing` goes to
   *  the sink from then on. */
  setLogSink(sink: (level: string, target: string, message: string) => void): void;
}

/** What every page-level chunk's loader does once `mod.default()` resolves, in
 *  one place rather than one copy per loader: hand the chunk's `tracing` to the
 *  core chunk's ring (each chunk is its own runtime with its own dispatcher, and
 *  only `fauna-wasm` holds the ring — `observability.md` § Persistence &
 *  privacy, the Web bullet), then register it with the identity store, whose
 *  key, if this page holds it, goes in before anything reads the registry
 *  through the chunk (`identity-store.ts`; a chunk without those doors is
 *  ignored there). Call it before publishing the module to callers. */
export function joinChunk(mod: JoinableChunk): void {
  mod.setLogSink(logMessage);
  registerIdentityStoreChunk(mod);
}
