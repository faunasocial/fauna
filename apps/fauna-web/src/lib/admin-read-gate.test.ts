// Deno test for how an admin control waits for its read (`admin-read-gate.ts`;
// `docs/goal/behavior/admin.md` § Errors & edge cases, *Controls wait for
// their read*). Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/admin-read-gate.test.ts
//
// The answers are shared Rust (`Possibility::from_read` / `for_write`, the
// admin machines' faces) and pinned there; the stand-ins below speak their
// serde form so these tests exercise web's own part — paint, the published
// reason, the list line and the confirm press — mirroring the per-page list
// of FaunaKit's `AdminControlsWaitForTheirReadTests.swift`.

import {
  beginConfirmPress,
  dispatchPainted,
  listReadReason,
  machineAnswer,
  machineGate,
  onReconnect,
  serialRead,
  reasonAttr,
  type Possibility,
  type ReadWord,
} from './admin-read-gate.ts';
import { afterGate, formGate, formRefusal } from './form-gate.ts';
import { t } from './i18n/strings.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

const blocked = (key: string): Possibility => ({ state: 'blocked', reason: { key } });
const possible: Possibility = { state: 'possible' };

/** `Possibility::from_read`'s stand-in. */
function fromRead(read: ReadWord): Possibility {
  if (read === 'pending') return blocked('common.still_loading');
  if (read === 'failed') return blocked('common.load_failed');
  return possible;
}

/** `Possibility::for_write`'s stand-in (`writeAfterReadPossibility`, and the
 *  shape every machine's write answer has). */
function forWrite(read: ReadWord, idle: boolean): Possibility {
  const first = fromRead(read);
  if (first.state !== 'possible') return first;
  return idle ? possible : blocked('common.in_flight');
}

// ── Every page: a write control is blocked before, after a failed read and in flight ──

Deno.test('a write control is still loading before its read, and refuses the press with it', () => {
  const gate = machineGate(() => forWrite('pending', true));
  eq(gate, { shown: true, disabled: true, reason: t.common.still_loading }, 'pending paint');
  eq(reasonAttr(gate), t.common.still_loading, 'pending reason attr');
  eq(formRefusal(forWrite('pending', true)), t.common.still_loading, 'pending press refused');
});

Deno.test('a failed read leaves the control blocked with failed to load (no guessed default saved)', () => {
  const gate = machineGate(() => forWrite('failed', true));
  eq(gate.reason, t.common.load_failed, 'failed paint');
  eq(formRefusal(forWrite('failed', true)), t.common.load_failed, 'failed press refused');
});

Deno.test('while its own change is on its way the control says still saving', () => {
  eq(machineGate(() => forWrite('resolved', false)).reason, t.common.in_flight, 'in flight paint');
  eq(formRefusal(forWrite('resolved', false)), t.common.in_flight, 'second press refused');
});

Deno.test('once read and idle the control is live and publishes no reason', () => {
  const gate = machineGate(() => forWrite('resolved', true));
  eq(gate, { shown: true, disabled: false, reason: '' }, 'live paint');
  eq(reasonAttr(gate), undefined, 'no reason attr when live');
  eq(formRefusal(forWrite('resolved', true)), null, 'press goes on');
});

Deno.test('with no machine yet (or the wasm core down) the control is disabled and the press still loading', () => {
  eq(machineGate(null), { shown: true, disabled: true, reason: '' }, 'no machine');
  eq(machineGate(() => { throw new Error('wasm not up'); }).disabled, true, 'wasm down');
});

Deno.test('a page whose machine could not be built says why: still loading, then failed to load', () => {
  const face = (_m: string) => possible;
  eq(formGate(machineAnswer<string>(null, face, 'pending', fromRead)).reason, t.common.still_loading, 'building');
  const failed = formGate(machineAnswer<string>(null, face, 'failed', fromRead));
  eq(failed, { shown: true, disabled: true, reason: t.common.load_failed }, 'build failed: disabled with its reason');
  eq(reasonAttr(failed), t.common.load_failed, 'the reason is published');
  eq(formRefusal(machineAnswer<string>(null, face, 'failed', fromRead)), t.common.load_failed, 'press refused');
  eq(machineAnswer('m', face, 'failed', fromRead), possible, 'a built machine answers for itself');
  eq(machineAnswer<string>(null, face, 'failed', () => { throw new Error('wasm not up'); }), null, 'wasm down');
});

// ── Mail / Calendar / Aliases / Settings / Nest: the read gate folds before the draft ──

Deno.test('a page still loading says so before a malformed draft', () => {
  const draft = () => blocked('admin.calendar_page.caldav_port_invalid');
  eq(afterGate(forWrite('pending', true), draft), blocked('common.still_loading'), 'read first');
  eq(afterGate(forWrite('resolved', true), draft), draft(), 'then the draft');
  eq(afterGate(forWrite('resolved', true), () => possible), possible, 'both possible');
});

// ── Bridges: list sections say loading / failed before their empty text ──

Deno.test('a list section says it is loading, then failed, and nothing once read', () => {
  eq(listReadReason(fromRead, 'pending'), t.common.still_loading, 'pending line');
  eq(listReadReason(fromRead, 'failed'), t.common.load_failed, 'failed line');
  eq(listReadReason(fromRead, 'resolved'), null, 'read: rows or empty text');
  eq(listReadReason(() => { throw new Error('wasm not up'); }, 'resolved'), t.common.still_loading, 'wasm down');
});

// ── Bridges: the confirm is refused with in flight on a second press ──

function fakeBridge() {
  let status: 'idle' | 'working' = 'idle';
  let armed = true;
  const m = {
    confirmPossibility(): Possibility {
      if (status !== 'idle') return blocked('common.in_flight');
      return armed ? possible : { state: 'absent' };
    },
    beginConfirm(): boolean {
      if (m.confirmPossibility().state !== 'possible') return false;
      status = 'working';
      return true;
    },
    finish() { status = 'idle'; armed = false; },
  };
  return m;
}

Deno.test('the first confirm press goes on; a second is refused with still saving', () => {
  const m = fakeBridge();
  eq(beginConfirmPress(m), null, 'first press dispatches');
  eq(beginConfirmPress(m), t.common.in_flight, 'second press refused in flight');
  eq(machineGate(() => m.confirmPossibility()).reason, t.common.in_flight, 'confirm paints in flight');
  m.finish();
  eq(beginConfirmPress(m), '', 'no armed confirm: nothing to add');
  eq(beginConfirmPress(null), t.common.still_loading, 'no machine: still loading');
});

Deno.test('dispatchPainted repaints after the dispatch started, before it resolves', async () => {
  const order: string[] = [];
  let release!: () => void;
  const result = await dispatchPainted(
    () => {
      order.push('start');
      queueMicrotask(() => order.push('first poll'));
      return new Promise<string>((r) => { release = () => r('done'); queueMicrotask(release); });
    },
    () => order.push('repaint'),
  );
  eq(result, 'done', 'resolves with the dispatch');
  eq(order, ['start', 'first poll', 'repaint'], 'repaint after the first poll');
});

Deno.test('reasonAttr is undefined for an empty or missing reason', () => {
  eq(reasonAttr(''), undefined, 'empty string');
  eq(reasonAttr(null), undefined, 'null');
  eq(reasonAttr('Failed to load'), 'Failed to load', 'a reason string');
  eq(reasonAttr(formGate(blocked('common.load_failed'))), t.common.load_failed, 'a gate');
});

Deno.test('serialRead never runs two reads at once and runs once more for a call made meanwhile', async () => {
  let running = 0;
  let maxRunning = 0;
  let runs = 0;
  const gates: (() => void)[] = [];
  const read = serialRead(async () => {
    running += 1;
    maxRunning = Math.max(maxRunning, running);
    runs += 1;
    await new Promise<void>((r) => gates.push(r));
    running -= 1;
  });
  const first = read();
  void read();
  void read();
  eq(runs, 1, 'the calls made while the read runs wait for it');
  gates.shift()!();
  await new Promise((r) => setTimeout(r, 0));
  eq(runs, 2, 'one more read after the first, for both calls made meanwhile');
  gates.shift()!();
  await first;
  eq(runs, 2, 'no third read');
  eq(maxRunning, 1, 'never two at once');
});

Deno.test('serialRead runs again after a read that threw', async () => {
  let runs = 0;
  const read = serialRead(async () => {
    runs += 1;
    throw new Error('nest down');
  });
  await read().catch(() => {});
  await read().catch(() => {});
  eq(runs, 2, 'a failed read does not wedge the runner');
});

Deno.test('onReconnect fires when the connection comes back, never on the opening word', () => {
  let push!: (word: string) => void;
  let unsubscribed = false;
  const subscribe = (run: (word: string) => void) => {
    push = run;
    run('connected');
    return () => {
      unsubscribed = true;
    };
  };
  let fired = 0;
  const stop = onReconnect(subscribe, () => {
    fired += 1;
  });
  eq(fired, 0, "the opening word is the entry read's");
  push('connected');
  eq(fired, 0, 'connected to connected is no return');
  push('disconnected');
  push('connecting');
  eq(fired, 0, 'going down reads nothing');
  push('connected');
  eq(fired, 1, 'coming back reads');
  stop();
  eq(unsubscribed, true, 'returns the unsubscribe');
});
