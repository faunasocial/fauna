// The two e2e doors of the dead-receive-rail witness (`conversations_mark_receive_stopped`
// / `conversations_begin_receive_loop`; `ui/conversations.md` § Errors & edge cases),
// kept apart from `conversations.ts` so the dispatch is unit-testable without the wasm
// manager or svelte. Both act on the same stalled flag `ReceivePump`'s `onStalledChange`
// drives, so the page renders `receive_stopped` exactly as for a real stall.

/** The slice of a svelte `Writable<boolean>` the doors need. */
export interface StalledFlag {
  set(stalled: boolean): void;
}

/** Stand up the dead receive rail — the stall the pump reports when a pass outruns its ceiling. */
export function markReceiveStoppedForTest(stalled: StalledFlag): void {
  stalled.set(true);
}

/** What a fresh pump pass does on recovery (`setStalled(false)`): retire the notice. */
export function beginReceiveLoopForTest(stalled: StalledFlag): void {
  stalled.set(false);
}
