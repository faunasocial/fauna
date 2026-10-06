// Pins for the pure half of the supervision-snapshot restore
// (family-safety.md § Content policy clause 2). Graded the way the row asks:
// each test kills a specific mutant of `applyRestoredSnapshot` — the
// happy-path pin alone would stay green over a restore that drops
// `content_notify` (the exact mutant that survived first on tui) or one that
// seeds a floor off a guardian-less snapshot (the graduation bug).

import {
  applyRestoredSnapshot,
  type RestoreSinks,
  type SupervisionSnapshotValue,
} from './supervisionRestore.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${JSON.stringify(actual)}, want ${JSON.stringify(expected)}`);
  }
}

type ContentCall = { policy: unknown; notify: boolean };
type ScreenCall = { policy: unknown; guardianHandle: string };

function recordingSinks() {
  const content: ContentCall[] = [];
  const screen: ScreenCall[] = [];
  const sinks: RestoreSinks = {
    restoreContentPolicy: (policy, notify) => content.push({ policy, notify }),
    restoreScreenTime: (policy, guardianHandle) => screen.push({ policy, guardianHandle }),
  };
  return { sinks, content, screen };
}

const floor = { nsfw: 'block', spam: 'inherit', phishing: 'inherit', commercial: 'inherit' };
const bedtime = { window_start: 1260, window_end: 420, daily_minutes: 90 };

function supervisedSnapshot(): SupervisionSnapshotValue {
  return {
    supervised_by: { actor_id_hex: 'abcd01', handle: 'parent@example.org' },
    // deno-lint-ignore no-explicit-any
    content_policy: floor as any,
    content_notify: true,
    // deno-lint-ignore no-explicit-any
    screen_time: bedtime as any,
  };
}

Deno.test('applyRestoredSnapshot — a supervised snapshot seeds all three pillars', () => {
  const { sinks, content, screen } = recordingSinks();
  const handle = applyRestoredSnapshot(supervisedSnapshot(), sinks);
  eq(handle, 'parent@example.org', 'the guardian handle drives the indicator + family-tab gate');
  eq(content.length, 1, 'the content half is seeded once');
  eq(content[0].policy, floor, 'the guardian floor is what a cold launch must restore');
  eq(content[0].notify, true, 'content_notify restores too — the tui mutant-7 field');
  eq(screen.length, 1, 'the screen-time half is seeded once');
  eq(screen[0].policy, bedtime, 'the bedtime window restores — airplane mode is not a bypass');
  eq(screen[0].guardianHandle, 'parent@example.org', 'the lock names the guardian');
});

Deno.test('applyRestoredSnapshot — a guardian-less snapshot seeds NOTHING (graduation direction)', () => {
  // A slot claiming a policy but no guardian cannot come from the shared fold
  // (its graduation gate strips policy fields when supervised_by is absent),
  // but the restore must not trust that: whatever wrote it, a floor may only
  // ever be restored under the guardianship that owns it.
  const snap = supervisedSnapshot();
  snap.supervised_by = null;
  const { sinks, content, screen } = recordingSinks();
  const handle = applyRestoredSnapshot(snap, sinks);
  eq(handle, null, 'no guardian, no indicator');
  eq(content.length, 0, 'no guardian, no floor');
  eq(screen.length, 0, 'no guardian, no lock');
});

Deno.test('applyRestoredSnapshot — absent snapshot is "no information", nothing seeds', () => {
  const { sinks, content, screen } = recordingSinks();
  const handle = applyRestoredSnapshot(undefined, sinks);
  eq(handle, null, 'nothing restored');
  eq(content.length + screen.length, 0, 'no sink ran');
});

Deno.test('applyRestoredSnapshot — supervised with no screen-time policy still names the guardian', () => {
  // A supervised ward whose guardian set no window/budget: the lock stays off
  // (null policy) but the guardian must still reach the screen-time store —
  // that is what a live read of the same account would establish.
  const snap = supervisedSnapshot();
  snap.screen_time = null;
  snap.content_notify = false;
  const { sinks, content, screen } = recordingSinks();
  const handle = applyRestoredSnapshot(snap, sinks);
  eq(handle, 'parent@example.org', 'supervised');
  eq(screen[0].policy, null, 'no window to enforce');
  eq(screen[0].guardianHandle, 'parent@example.org', 'the store still learns the guardian');
  eq(content[0].notify, false, 'notify off restores as off, not as a dropped field');
});
