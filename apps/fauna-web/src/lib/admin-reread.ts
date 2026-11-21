// When an admin page reads again (`docs/goal/behavior/admin.md` § Persistence,
// every page re-reads on entry; § Errors & edge cases, *Controls wait for their
// read*): on every navigation to it — `afterNavigate`, which also fires on
// mount, and fires for a `goto()` of the route already shown, which does not
// remount the page — and, for a read still unanswered, when the nest connection
// comes back. One trigger for every admin page with a read gate. The pure
// halves (`serialRead`, `onReconnect`) live in `admin-read-gate.ts`, where
// plain `deno test` reaches them; this module only wires them to SvelteKit and
// the connection store. Call it during component initialisation.

import { afterNavigate } from '$app/navigation';
import { onDestroy } from 'svelte';
import { onReconnect, serialRead } from './admin-read-gate';
import { connectionStatus } from './store';

/** `entry` runs on every navigation to the page (mount included);
 *  `reconnect` — the page's reads that have not answered yet, `entry` when
 *  omitted — when the nest connection comes back. Both go through one
 *  `serialRead`, so a page never has two reads on their way at once: a
 *  reconnect while the entry read runs waits for it and runs once after it,
 *  and an entry asked meanwhile wins over a reconnect. */
export function readOnEntryAndReconnect(
  entry: () => Promise<void>,
  reconnect: () => Promise<void> = entry,
): void {
  let wantEntry = false;
  const run = serialRead(async () => {
    const whole = wantEntry;
    wantEntry = false;
    await (whole ? entry() : reconnect());
  });
  afterNavigate(() => {
    wantEntry = true;
    void run();
  });
  onDestroy(onReconnect(connectionStatus.subscribe, () => void run()));
}
