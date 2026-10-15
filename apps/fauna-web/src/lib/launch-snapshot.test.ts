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

import {
  supersededSuccessorOf,
  offlineTransientOf,
  identityForkOf,
  lockedUntilOf,
} from './launch-snapshot.ts';

// The fork variant of the identity-changed surface (`box-recovery.md` § Client
// acceptance): the `identity_fork` bit only flags it, the two heads come from the
// `IdentityChanged` phase. The e2e arm is unreachable on web (its origin is never
// TLS, so the native-binding fork arm cannot fire), so these are the witness.
Deno.test('a fork snapshot yields both heads from the IdentityChanged phase', () => {
  const pinned = 'aa'.repeat(32);
  const seen = 'bb'.repeat(32);
  const got = identityForkOf({
    phase: { IdentityChanged: { pinned_hex: pinned, seen_hex: seen } },
    identity_fork: true,
  });
  if (!got || got.pinnedHex !== pinned || got.seenHex !== seen) {
    throw new Error(`expected both heads, got ${JSON.stringify(got)}`);
  }
});

Deno.test('without the fork bit the identity-changed phase is not a fork', () => {
  for (const bit of [undefined, false]) {
    const got = identityForkOf({
      phase: { IdentityChanged: { pinned_hex: 'aa', seen_hex: 'bb' } },
      identity_fork: bit,
    });
    if (got !== null) throw new Error(`bit ${bit} must keep the ordinary surface`);
  }
});

Deno.test('the fork bit alone, or a withdrawn identity, is not a renderable fork', () => {
  if (identityForkOf({ phase: 'Online', identity_fork: true }) !== null) {
    throw new Error('a fork needs the IdentityChanged phase to name its heads');
  }
  const withdrawn = identityForkOf({
    phase: { IdentityChanged: { pinned_hex: 'aa', seen_hex: null } },
    identity_fork: true,
  });
  if (withdrawn !== null) throw new Error('a withdrawn identity has no second head');
});

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

// The locked surface (`ui/sessions.md` § Layout & flow, leg 2) keys on the
// side channel, never the phase: a lock parks the machine on the same
// `Offline { transient: false }` the outdated-nest row uses.
Deno.test('a locked snapshot yields its unlock time; an unlocked one yields null', () => {
  const locked = lockedUntilOf({ phase: { Offline: { transient: false } }, locked_until_secs: 1700086400 });
  if (locked !== 1700086400) throw new Error(`expected the unlock time, got ${locked}`);
  for (const snap of [
    { phase: { Offline: { transient: false } } },
    { phase: { Offline: { transient: false } }, locked_until_secs: null },
    { phase: 'Online', locked_until_secs: undefined },
  ]) {
    const got = lockedUntilOf(snap);
    if (got !== null) throw new Error(`expected null for ${JSON.stringify(snap)}, got ${got}`);
  }
});

