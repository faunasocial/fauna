// The **store-change notice** in the SPA — web's consumer of the one shared
// watch (`fauna_account_seams::store_change`, crossed as
// `accountStoreChangedAfter`; `docs/goal/architecture/account-runtime.md`
// § Multi-instance concurrency → *A runtime's own pump is a source of the
// notice too*, parts 4 and 5): an OPEN page whose render source is read
// through the account store shows what a fresh visit would show, whoever
// changed the store.
//
// ONE relay per running runtime (`$lib/account-runtime` starts it where the
// runtime becomes ready; it ends by itself when the runtime stops) and a
// listener set the open pages join — a page never calls wasm for the notice.
// Only the tab that hosts the runtime hears anything: a second tab of the
// same account runs no pump and IndexedDB has no cross-connection counter, so
// it has no source until the cross-tab poke is built.
//
// The notice is payload-free and a level, not an event: a listener re-runs its
// page's OWN load, paints only what differs and never discards an edit in
// progress. **A new store-backed page joins with `onStoreChange` in the change
// that adds it.** A gesture's own write is not a source — the page that made
// it repaints from the gesture's answer.

type Listener = () => void;

const listeners = new Set<Listener>();

/** Re-run `listener` whenever the account store may have changed. Returns the
 *  unsubscribe — the shape a Svelte `$effect` or `onMount` returns as its
 *  teardown, so a page is joined exactly while it is open. */
export function onStoreChange(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Relay one runtime's notices to the open pages until that runtime is gone.
 *  `changedAfter` is the wasm face: it resolves with the notice count once it
 *  differs from the one passed, `undefined` when the runtime stopped (or this
 *  tab hosts none). A listener that throws, or a face that rejects, is handed
 *  to `fault` and never stops the other listeners. */
export async function relayStoreChanges(
  changedAfter: (seen: number) => Promise<number | undefined>,
  fault: (e: unknown) => void,
): Promise<void> {
  let seen = 0;
  for (;;) {
    let count: number | undefined;
    try {
      count = await changedAfter(seen);
    } catch (e) {
      fault(e);
      return;
    }
    if (count === undefined) return;
    seen = count;
    for (const listener of [...listeners]) {
      try {
        listener();
      } catch (e) {
        fault(e);
      }
    }
  }
}
