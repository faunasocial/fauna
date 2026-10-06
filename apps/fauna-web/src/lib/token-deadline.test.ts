// Deno tests for the SPA's bearer-deadline anchor. Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/token-deadline.test.ts
//
// Web's leg of `docs/goal/behavior/login.md` § Token lifetime on the client's
// clock. The same rule is pinned Rust-side on the one conversion every other
// holder uses (`fauna_protocol::auth::deadline_on_own_clock`); web's bearer
// cache is the one seat that is not Rust, so its copy is pinned here.

import { deadlineOnOwnClock } from './token-deadline.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) throw new Error(`${msg}: got ${actual}, want ${expected}`);
}

const NEST_NOW = 1_800_000_000;

Deno.test('a device six hours ahead anchors on its own clock', () => {
  // Read against the nest's deadline, this device would see a token five hours
  // dead and re-mint on every request.
  const clientNow = NEST_NOW + 6 * 3600;
  eq(deadlineOnOwnClock(clientNow, 3600, NEST_NOW + 3600), clientNow + 3600, 'ahead');
});

Deno.test('a device six hours behind anchors on its own clock', () => {
  // Read against the nest's deadline, this device would serve a token for
  // six hours after the nest stopped honouring it.
  const clientNow = NEST_NOW - 6 * 3600;
  eq(deadlineOnOwnClock(clientNow, 3600, NEST_NOW + 3600), clientNow + 3600, 'behind');
});

Deno.test('the nest deadline never overrides the anchored one', () => {
  // The older-nest fallback (no `expires_in` → `expires_at`) retired
  // 2026-09-24; `expires_in` is required and always wins.
  eq(deadlineOnOwnClock(NEST_NOW, 3600, NEST_NOW + 99_999), NEST_NOW + 3600, 'anchored');
});
