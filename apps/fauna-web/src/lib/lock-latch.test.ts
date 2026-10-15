// Run with:
//     deno test --allow-read --no-check apps/fauna-web/src/lib/lock-latch.test.ts
//
// The web twin of the native bearer mints' locked latch
// (`ws_challenge_bearer::tests::a_standing_lock_is_answered_without_dialling`;
// `devices.md` § The locked state): a mint refused `fauna.auth.account_locked`
// is answered from the latch until the unlock time, so a locked tab stops
// asking the nest — the locked-surface witness counts every refusal.

import { LockLatch } from './lock-latch.ts';

Deno.test('a latched lock is answered until its unlock time, then lapses', () => {
  const latch = new LockLatch();
  latch.latch('k', 1000);
  if (latch.standing('k', 999) !== 1000) throw new Error('a lock before its time must stand');
  if (latch.standing('k', 1000) !== null) throw new Error('a lock at its time has ended');
  if (latch.standing('k', 1001) !== null) throw new Error('a lock past its time has ended');
});

Deno.test('a lock is per key — another nest or identity is not held', () => {
  const latch = new LockLatch();
  latch.latch('k', 1000);
  if (latch.standing('other', 10) !== null) throw new Error('an unlatched key must not stand');
});

Deno.test('a successful mint clears the latch', () => {
  const latch = new LockLatch();
  latch.latch('k', 1000);
  latch.clear('k');
  if (latch.standing('k', 10) !== null) throw new Error('a cleared latch must not stand');
});
