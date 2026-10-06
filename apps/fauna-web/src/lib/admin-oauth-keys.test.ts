// Deno tests for the outside-app sign-in key section's guards
// (`admin-nest-oauth-*`; authorization-server.md § The issuer → Two rotation
// arms). Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/admin-oauth-keys.test.ts
//
// These pin WHEN a control may act, never what it says: every sentence is a
// shared `fauna_client_admin` fold, so the confirm fold is injected as a stub
// that records its calls. The same guards as tui's gesture tests
// (`apps/fauna-tui/src/admin/mod.rs`): nothing acts before the key set has
// answered or while a call is in flight, arming dispatches nothing and folds
// once, and a confirm press disarms BEFORE it dispatches, so a double press
// cannot drop the key the first press minted.

import {
  armOauthForced,
  beginOauthRotate,
  cancelOauthForced,
  finishOauthCall,
  initialOauthSection,
  oauthKeys,
  oauthKeysLoaded,
  oauthLive,
  takeOauthConfirm,
  type IssuerForcedArm,
  type IssuerForcedConfirmView,
  type IssuerKeyView,
  type OauthKeysRead,
  type OauthSection,
} from './admin-oauth-keys.ts';

function assert(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

function assertEq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}: expected ${e}, got ${a}`);
}

const WORKING = 'WORKING';

function view(n: number): IssuerKeyView {
  return {
    active_kid: 'k0',
    keys: Array.from({ length: n }, (_, i) => ({
      kid: `k${i}`,
      signing: i === 0,
      retired_at: i === 0 ? null : 1_700_000_000,
      served_until: i === 0 ? null : 1_700_001_200,
    })),
    retirement_horizon_secs: 1200,
    rotation_in_flight: n > 1,
  };
}

function ready(n: number): OauthSection {
  return oauthKeysLoaded(initialOauthSection(), { kind: 'ready', view: view(n) });
}

/** A confirm fold stub that names its arm and records every call. */
function recordingFold() {
  const calls: Array<{ arm: IssuerForcedArm; keys: number }> = [];
  const fold = (arm: IssuerForcedArm, v: IssuerKeyView): IssuerForcedConfirmView => {
    calls.push({ arm, keys: v.keys.length });
    return {
      summary: { key: `summary.${arm}`, args: { count: String(v.keys.length) } },
      confirm_label: { key: `label.${arm}`, args: {} },
    };
  };
  return { calls, fold };
}

const UNANSWERED: OauthKeysRead[] = [
  { kind: 'unread' },
  { kind: 'failed', reason: 'unknown kind' },
];

Deno.test('a fresh section is unread, unarmed, silent and idle', () => {
  const s = initialOauthSection();
  assertEq(s.keys, { kind: 'unread' }, 'fresh read state');
  assertEq(s.armed, null, 'fresh armed state');
  assertEq(s.status, null, 'fresh status');
  assert(!s.inFlight, 'a fresh section has no call in flight');
  assertEq(oauthKeys(s), null, 'an unread set has no view to paint');
  assert(!oauthLive(s), 'the controls are disabled before the set answers');
});

Deno.test('only an answered read yields a view, and it is the view as given (signer first)', () => {
  for (const keys of UNANSWERED) {
    const s = oauthKeysLoaded(initialOauthSection(), keys);
    assertEq(oauthKeys(s), null, `${keys.kind} must paint no key rows`);
    assert(!oauthLive(s), `${keys.kind} must leave every control disabled`);
  }
  const s = ready(2);
  assertEq(
    oauthKeys(s)?.keys.map((k) => k.kid),
    ['k0', 'k1'],
    'the rows keep the nest order — never re-sorted',
  );
  assert(oauthLive(s), 'an answered, idle set enables the controls');
});

Deno.test('nothing acts before the set has answered', () => {
  for (const keys of UNANSWERED) {
    const s = oauthKeysLoaded(initialOauthSection(), keys);
    assertEq(beginOauthRotate(s, WORKING), null, `rotate must refuse on ${keys.kind}`);
    const { calls, fold } = recordingFold();
    for (const arm of ['IssuerKey', 'SessionSecret'] as const) {
      assertEq(armOauthForced(s, arm, fold), null, `${arm} must not arm on ${keys.kind}`);
    }
    assertEq(calls.length, 0, 'a refused arm must not fold a confirm');
  }
});

Deno.test('the ordinary rotation disarms a stale confirm, goes in flight, and says so', () => {
  const { fold } = recordingFold();
  const armed = armOauthForced(ready(2), 'IssuerKey', fold);
  assert(armed !== null, 'precondition: the key arm arms');
  const began = beginOauthRotate(armed!, WORKING);
  assert(began !== null, 'an answered, idle set rotates');
  assertEq(began!.armed, null, 'the rotation disarms the confirm whose key count it changes');
  assert(began!.inFlight, 'the rotation is in flight');
  assertEq(began!.status, WORKING, 'the status line says the call is working');
  assert(!oauthLive(began!), 'every control is disabled while the call is in flight');
  assertEq(beginOauthRotate(began!, WORKING), null, 'a second press while in flight dispatches nothing');
});

Deno.test('arming folds ONCE over the current view, clears the status, and dispatches nothing', () => {
  const { calls, fold } = recordingFold();
  const s = { ...ready(2), status: 'a previous verdict' };
  const armed = armOauthForced(s, 'IssuerKey', fold);
  assert(armed !== null, 'the key arm arms');
  assertEq(calls, [{ arm: 'IssuerKey', keys: 2 }], 'the fold runs once, over the set the admin sees');
  assertEq(armed!.armed?.arm, 'IssuerKey', 'the armed arm');
  assertEq(armed!.armed?.confirm.summary.key, 'summary.IssuerKey', 'the captured confirm');
  assertEq(armed!.status, null, 'arming clears the previous verdict');
  assert(!armed!.inFlight, 'arming dispatches nothing');
  // One arm at a time: arming the sibling replaces the confirm outright.
  const sibling = armOauthForced(armed!, 'SessionSecret', fold);
  assertEq(sibling!.armed?.arm, 'SessionSecret', 'the sibling arm replaces the first');
  assertEq(sibling!.armed?.confirm.summary.key, 'summary.SessionSecret', 'its own confirm');
});

Deno.test('the armed confirm is captured, never re-folded when the set moves on', () => {
  const { calls, fold } = recordingFold();
  const armed = armOauthForced(ready(2), 'IssuerKey', fold)!;
  const moved = oauthKeysLoaded(armed, { kind: 'ready', view: view(3) });
  assertEq(moved.armed?.confirm.summary.args, { count: '2' }, 'the confirm still names what it was armed over');
  assertEq(calls.length, 1, 'no second fold');
});

Deno.test('no arm while a call is in flight', () => {
  const { calls, fold } = recordingFold();
  const began = beginOauthRotate(ready(1), WORKING)!;
  for (const arm of ['IssuerKey', 'SessionSecret'] as const) {
    assertEq(armOauthForced(began, arm, fold), null, `${arm} must not arm in flight`);
  }
  assertEq(calls.length, 0, 'a refused arm must not fold');
});

Deno.test('cancel disarms and touches nothing else', () => {
  const { fold } = recordingFold();
  const armed = armOauthForced(ready(2), 'SessionSecret', fold)!;
  const cancelled = cancelOauthForced(armed);
  assertEq(cancelled.armed, null, 'cancel disarms');
  assertEq(cancelled.keys, armed.keys, 'cancel keeps the set');
  assertEq(cancelled.status, armed.status, 'cancel writes no verdict');
  assert(!cancelled.inFlight, 'cancel dispatches nothing');
});

Deno.test('a confirm press disarms BEFORE it dispatches, so a double press dispatches once', () => {
  const { fold } = recordingFold();
  const armed = armOauthForced(ready(2), 'IssuerKey', fold)!;
  const taken = takeOauthConfirm(armed, 'IssuerKey', WORKING);
  assert(taken !== null, 'the armed confirm dispatches');
  assertEq(taken!.arm, 'IssuerKey', 'it dispatches exactly the armed arm');
  assertEq(taken!.state.armed, null, 'the confirm is gone the moment it is pressed');
  assert(taken!.state.inFlight, 'the forced call is in flight');
  assertEq(taken!.state.status, WORKING, 'the status line says the call is working');
  assertEq(takeOauthConfirm(taken!.state, 'IssuerKey', WORKING), null, 'the second press finds nothing armed');
});

Deno.test('a confirm painted for one arm dispatches nothing once the other is armed', () => {
  const { fold } = recordingFold();
  const armed = armOauthForced(ready(2), 'SessionSecret', fold)!;
  assertEq(takeOauthConfirm(armed, 'IssuerKey', WORKING), null, 'a mismatched confirm refuses');
  assertEq(armed.armed?.arm, 'SessionSecret', 'and what the admin can see stays armed');
});

Deno.test('a confirm refuses while a call is in flight and keeps the confirm armed', () => {
  const { fold } = recordingFold();
  // Unreachable through the controls (an in-flight call refuses every arm),
  // but the confirm's own guard must still hold on its own.
  const s: OauthSection = { ...armOauthForced(ready(1), 'IssuerKey', fold)!, inFlight: true };
  assertEq(takeOauthConfirm(s, 'IssuerKey', WORKING), null, 'no dispatch in flight');
  assertEq(s.armed?.arm, 'IssuerKey', 'the armed confirm is kept');
});

Deno.test('a call ends with the verdict and the re-read landing together', () => {
  const began = beginOauthRotate(ready(1), WORKING)!;
  const done = finishOauthCall(began, 'VERDICT', { kind: 'ready', view: view(2) });
  assertEq(done.status, 'VERDICT', 'the verdict replaces the working line');
  assertEq(oauthKeys(done)?.keys.length, 2, 'the re-read set lands with it');
  assert(!done.inFlight, 'the call is over');
  assert(oauthLive(done), 'the controls come back live');
  // A failed re-read after a call keeps the verdict and blanks the rows.
  const lost = finishOauthCall(began, 'VERDICT', { kind: 'failed', reason: 'gone' });
  assertEq(lost.status, 'VERDICT', 'the verdict still stands');
  assertEq(oauthKeys(lost), null, 'no rows from a failed re-read');
  assert(!oauthLive(lost), 'and the controls wait for a set again');
});
