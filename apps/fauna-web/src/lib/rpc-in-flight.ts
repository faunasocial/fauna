// How many WS-RPC calls this tab has in flight — the count `#lib/rpc`'s `call`
// and `callStore` keep, from the call's entry (before its client and connect
// waits) to its settling.
//
// Its one reader is the e2e `barrier` (`#lib/barrier-e2e`): a gesture whose
// handler awaits an RPC paints its outcome only once that call settles, which
// a fixed number of macrotask turns cannot outrun on a loaded box — so the
// barrier waits for this count to read zero (convention 14's corollary: work
// that outlives the barrier's turns needs a completion observable of its own,
// `docs/goal/architecture/e2e-conventions.md`).
//
// It lives in the production bundle because `#lib/rpc` does: two integer
// writes per call, no listener, no allocation. Nothing in the product reads it.
// The second writer, `countedFace` (the conversations manager's gesture-awaited
// calls, which reach the nest through wasm and never pass `call`), is wired
// only in test builds.
// The test-only barrier may turn on `traceRpcInFlight`, which records each
// call's start and origin so a barrier that meets its bound can say which call
// held it; a production build never turns it on.

let inFlight = 0;

interface Traced {
  started: number;
  origin: string;
}
let traced: Set<Traced> | null = null;

/** Test builds only (`#lib/barrier-e2e`): record each call from now on. */
export function traceRpcInFlight(): void {
  traced ??= new Set();
}

/** Count `work` as in flight from now until it settles, either way. The count
 *  rises synchronously, so a barrier issued in the same task already sees it. */
export function trackRpc<T>(work: () => Promise<T>): Promise<T> {
  inFlight += 1;
  let entry: Traced | null = null;
  if (traced) {
    entry = { started: Date.now(), origin: (new Error().stack ?? '').split('\n').slice(2, 5).join(' | ') };
    traced.add(entry);
  }
  const done = () => {
    inFlight -= 1;
    if (entry) traced?.delete(entry);
  };
  let p: Promise<T>;
  try {
    p = work();
  } catch (e) {
    done();
    throw e;
  }
  return p.finally(done);
}

const countedFaces = new WeakMap<object, object>();

/** Test builds only: `target` behind a face that counts every promise one of
 *  its methods returns as in flight until it settles — for an engine object
 *  whose calls do nest work outside `#lib/rpc`'s counted `call` (the wasm
 *  conversations manager: `#lib/conversations`'s `getConversationsManager`).
 *  Each method runs on `target` itself, so the wasm glue sees its own object;
 *  a method returning no promise is passed through uncounted. One face per
 *  target, so two callers holding the manager hold the same object. */
export function countedFace<T extends object>(target: T): T {
  const known = countedFaces.get(target);
  if (known) return known as T;
  const face = new Proxy(target, {
    get(t, prop) {
      const value: unknown = Reflect.get(t, prop, t);
      if (typeof value !== 'function') return value;
      return (...args: unknown[]) => {
        const out: unknown = Reflect.apply(value as (...a: unknown[]) => unknown, t, args);
        if (out instanceof Promise) return trackRpc(() => out);
        return out;
      };
    },
  });
  countedFaces.set(target, face);
  return face;
}

/** The calls in flight right now. */
export function rpcInFlight(): number {
  return inFlight;
}

/** The oldest traced calls still in flight, for a barrier's bound message. */
export function oldestRpcInFlight(n = 3): string[] {
  if (!traced) return [];
  const now = Date.now();
  return [...traced]
    .sort((a, b) => a.started - b.started)
    .slice(0, n)
    .map((t) => `${now - t.started} ms: ${t.origin}`);
}
