// Deno test for `resolveGateSelection`. Run via:
//
//     deno test apps/fauna-web/src/lib/compose-gate.test.ts

import { resolveGateSelection } from './compose-gate.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

const PUBLIC = { gateTier: '', gateRoom: '', sellSelected: false };
const SELL = { gateTier: '', gateRoom: '', sellSelected: true };
const tier = (name: string) => ({ gateTier: name, gateRoom: '', sellSelected: false });
const room = (id: string) => ({ gateTier: '', gateRoom: id, sellSelected: false });

Deno.test('resolveGateSelection — index 0 is Public', () => {
  eq(resolveGateSelection(0, ['gold', 'silver'], ['aa']), PUBLIC, 'index 0 → Public regardless of own tiers and rooms');
});

Deno.test('resolveGateSelection — indices 1..N map onto own tiers by position', () => {
  const names = ['gold', 'silver', 'bronze'];
  eq(resolveGateSelection(1, names), tier('gold'), 'index 1 → first tier');
  eq(resolveGateSelection(3, names), tier('bronze'), 'index 3 → last tier');
});

Deno.test('resolveGateSelection — the last index is always sell, regardless of tier count', () => {
  eq(resolveGateSelection(1, []), SELL, 'no tiers: index 1 is sell');
  eq(resolveGateSelection(4, ['a', 'b', 'c']), SELL, '3 tiers: index 4 is sell');
});

Deno.test(
  'resolveGateSelection — rooms follow the tiers by position, and sell stays last (feed.md § Room-restricted — the app half)',
  () => {
    const tiers = ['gold'];
    const rooms = ['aa', 'bb'];
    eq(resolveGateSelection(1, tiers, rooms), tier('gold'), 'index 1 → the tier');
    eq(resolveGateSelection(2, tiers, rooms), room('aa'), 'index 2 → the first room, by its channel id');
    eq(resolveGateSelection(3, tiers, rooms), room('bb'), 'index 3 → the second room');
    eq(resolveGateSelection(4, tiers, rooms), SELL, 'one past the rooms → sell');
    eq(resolveGateSelection(2, [], ['aa']), SELL, 'no tiers, one room: index 2 is sell');
  },
);

Deno.test(
  'resolveGateSelection — a tier NAMED exactly the sell label does not hijack the sell branch',
  () => {
    // tiers.create has no reserved-name check, so an author can create a real
    // tier whose name is byte-identical to the localized "Sell this post…"
    // label. Both options would then render (and compare-by-value) as the
    // same string — the bug this fix removes. Resolution here is purely
    // positional, so the collision is inert: whichever entry the select's
    // index points at wins, never a string match.
    const SELL_LABEL = 'Sell this post…';
    const names = ['gold', SELL_LABEL, 'bronze'];
    // Index 2 is the SECOND own tier — the one named exactly like "sell" —
    // and must resolve as that TIER, not as sell.
    eq(
      resolveGateSelection(2, names),
      tier(SELL_LABEL),
      'picking the tier that shares the sell label must gate to that tier, not trigger sell',
    );
    // Index 4 (names.length + 1) is the true, structurally-last "sell"
    // option, unaffected by the collision two positions earlier.
    eq(resolveGateSelection(4, names), SELL, 'the actual last-position sell option must still resolve as sell');
  },
);

Deno.test('resolveGateSelection — a tier named like a room option stays the tier', () => {
  // The same positional rule as the sell collision above: a tier can carry a
  // room option's exact label, and still resolves as the tier it is.
  const names = ['Room: Crew'];
  eq(resolveGateSelection(1, names, ['bb']), tier('Room: Crew'), 'the tier at index 1');
  eq(resolveGateSelection(2, names, ['bb']), room('bb'), 'the real room at index 2');
});
