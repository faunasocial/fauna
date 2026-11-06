// Deno test for `LoopWake` — the web twin of tui's `loop_wake::LoopWake`: the
// e2e poke that ends one slow loop's wait early (convention 14). Run via:
//
//     deno test apps/fauna-web/src/lib/loop-wake.test.ts

import { LoopWake } from './loop-wake.ts';

const SIX_HOURS_MS = 6 * 60 * 60 * 1000;

/** Resolves `true` if `p` settles within `ms`, else `false` — never leaves the
 *  six-hour timer behind (the wake under test clears it). */
async function settlesWithin(p: Promise<void>, ms: number): Promise<boolean> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<boolean>((resolve) => {
    timer = setTimeout(() => resolve(false), ms);
  });
  const won = await Promise.race([p.then(() => true), timeout]);
  clearTimeout(timer);
  return won;
}

Deno.test('a wake mid-wait ends it long before its clock', async () => {
  const wake = new LoopWake();
  const waiting = wake.sleep(SIX_HOURS_MS);
  wake.wake();
  if (!(await settlesWithin(waiting, 1000))) throw new Error('the wake did not end the wait');
});

Deno.test('a wake that lands before the wait begins is kept for it', async () => {
  const wake = new LoopWake();
  wake.wake();
  if (!(await settlesWithin(wake.sleep(SIX_HOURS_MS), 1000))) {
    throw new Error('the stored wake did not end the next wait');
  }
});

Deno.test('one wake ends one wait, not the one after it', async () => {
  const wake = new LoopWake();
  wake.wake();
  wake.wake(); // a second poke while one is stored is the same single permit
  await wake.sleep(SIX_HOURS_MS);
  const next = wake.sleep(50);
  // The 50 ms clock, not a leftover permit, ends this one: it is still pending
  // after 10 ms.
  if (await settlesWithin(next, 10)) throw new Error('a consumed wake ended a second wait');
  await next;
});

Deno.test('the clock alone ends a wait nobody wakes', async () => {
  const wake = new LoopWake();
  if (!(await settlesWithin(wake.sleep(20), 1000))) throw new Error('the clock never ended the wait');
});
