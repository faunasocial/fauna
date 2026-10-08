// Deno tests for the held-back own-ceremony supersession. Run via:
//
//     just web-unit-test
//
// Web's leg of `docs/goal/ui/settings.md` § Recovery kit → *The persist-failure
// message survives the page*, closing rule: a supersession this device's own
// stolen-identity ceremony caused is held back while the ceremony runs or its
// persist-failure message is parked, and performed once the user leaves
// Account. tui's `App::defer_own_supersession` is the model (pinned there by
// `the_ceremonys_own_supersession_waits_for_the_parked_key`).

import { OwnSupersessionHold } from './own-supersession-hold.ts';

function expect(cond: boolean, msg: string): void {
  if (!cond) throw new Error(msg);
}

Deno.test('with no ceremony, a supersession is not held', () => {
  const hold = new OwnSupersessionHold();
  expect(!hold.defer(), 'nothing owns the supersession, so it escalates at once');
  expect(!hold.leftAccount(), 'and nothing is owed afterwards');
});

Deno.test('a supersession mid-ceremony is held, and owed only once', () => {
  const hold = new OwnSupersessionHold();
  hold.ceremonyStarted();
  expect(hold.defer(), 'the ceremony owns it');
  expect(hold.defer(), 'a second channel reporting the same refusal is held too');
  expect(!hold.ceremonyEnded({ onAccount: true, parked: false }), 'on Account the fold message is read first');
  expect(hold.leftAccount(), 'leaving Account performs the owed escalation');
  expect(!hold.leftAccount(), 'exactly once');
});

Deno.test('the parked key holds the escalation until the user leaves Account', () => {
  const hold = new OwnSupersessionHold();
  hold.ceremonyStarted();
  expect(!hold.ceremonyEnded({ onAccount: true, parked: true }), 'the key is on screen; nothing escalates');
  // The refusal has no fixed order against the fold: it may land after it.
  expect(hold.defer(), 'a refusal arriving after the fold is still the ceremony\'s while the key is parked');
  expect(hold.leftAccount(), 'leaving Account performs it');
});

Deno.test('a parked key holds the escalation even when the ceremony ended off Account', () => {
  const hold = new OwnSupersessionHold();
  hold.ceremonyStarted();
  hold.defer();
  expect(!hold.ceremonyEnded({ onAccount: false, parked: true }), 'escalating would tear the parked key down');
  expect(hold.leftAccount(), 'performed on the edge that discharges the key');
});

Deno.test('a ceremony ending off Account with nothing parked performs the owed escalation at once', () => {
  const hold = new OwnSupersessionHold();
  hold.ceremonyStarted();
  hold.defer();
  expect(hold.ceremonyEnded({ onAccount: false, parked: false }), 'nothing on screen needs the session');
  expect(!hold.defer(), 'and the ceremony owns no later supersession');
});

Deno.test('a ceremony that ended on Account with nothing owed keeps owning a late refusal', () => {
  const hold = new OwnSupersessionHold();
  hold.ceremonyStarted();
  expect(!hold.ceremonyEnded({ onAccount: true, parked: false }), 'nothing owed yet');
  expect(hold.defer(), 'the fold\'s message is on screen: a late refusal waits for the edge');
  expect(hold.leftAccount(), 'and the edge performs it');
  expect(!hold.defer(), 'off Account, a later supersession is no longer the ceremony\'s');
});

Deno.test('an adopted successor spends the owed escalation — the switch is the relaunch', () => {
  const hold = new OwnSupersessionHold();
  hold.ceremonyStarted();
  hold.defer();
  hold.adopted();
  expect(!hold.leftAccount(), 'nothing is owed once the switch relaunched');
  expect(!hold.defer(), 'and nothing is held');
});
