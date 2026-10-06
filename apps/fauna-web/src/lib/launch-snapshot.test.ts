// Deno tests for the launch snapshot's pure accessors. Run via:
//
//     just web-unit-test
//
// `supersededSuccessorOf` is the discriminator the succeeded-identity routing
// keys on, and it has to be a *field* read rather than a phase read: the launch
// machine projects `State::Superseded` to `Offline { transient: false }` — the
// same phase as the nest-outdated row — and carries the successor on an
// additive side channel so that an app which cannot yet render it still stops
// retrying (`fauna-launch-machine/src/snapshots.rs`). Routing off the phase
// alone is exactly the bug this closes: web showed a succeeded user "update
// your nest".

import { supersededSuccessorOf, offlineTransientOf } from './launch-snapshot.ts';

Deno.test('a superseded snapshot yields the claimed successor', () => {
  const successor = 'ab'.repeat(32);
  const got = supersededSuccessorOf({
    phase: { Offline: { transient: false } },
    superseded_successor: successor,
  });
  if (got !== successor) throw new Error(`expected ${successor}, got ${got}`);
});

Deno.test('the nest-outdated row shares the phase but names no successor', () => {
  // The regression guard for the two states being phase-indistinguishable: both
  // are `Offline { transient: false }`, and only the side channel separates
  // "your identity was succeeded" from "your nest is out of date".
  const snap = { phase: { Offline: { transient: false } }, last_error: 'outdated' };
  if (offlineTransientOf(snap) !== false) {
    throw new Error('the nest-outdated row must still be Offline{transient:false}');
  }
  if (supersededSuccessorOf(snap) !== null) {
    throw new Error('a snapshot with no side-channel successor must yield null');
  }
});

Deno.test('an absent, null or empty successor is null, never a falsy string', () => {
  // `null` is what the caller branches on; an empty string would route to the
  // import screen and then name nobody, which is worse than not routing at all.
  for (const value of [undefined, null, '']) {
    const got = supersededSuccessorOf({
      phase: { Offline: { transient: false } },
      superseded_successor: value,
    });
    if (got !== null) throw new Error(`expected null for ${JSON.stringify(value)}, got ${got}`);
  }
});

Deno.test('an Online snapshot names no successor', () => {
  if (supersededSuccessorOf({ phase: 'Online' }) !== null) {
    throw new Error('a healthy launch must never route to the import screen');
  }
});
