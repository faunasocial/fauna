import { NotifyBufferState } from './familyNotifyBuffer.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

const HOUR = 3600;

// The rules these pin are the shared ones linux's `NotifyAccumulator` tests pin
// (`apps/fauna-linux/src/content_policy.rs`), so the two apps cannot drift on
// what Guardian Notify reports.

Deno.test('notify buffer — dedups per item per local day and batches hourly', () => {
  const buf = new NotifyBufferState();
  eq(buf.record('post-1', ['spam'], 1000, 120), true, 'first enforcement counts');
  // The same item re-rendered must not re-count.
  eq(buf.record('post-1', ['spam'], 1005, 120), false, 're-render did not re-count');
  buf.record('post-2', ['spam'], 1010, 120);

  // The first report is eager (no prior flush) and carries the delta + offset.
  eq(buf.takeDue(1010, HOUR), { entries: [{ category: 'spam', count: 2 }], offsetMinutes: 120 },
    'first batch is due');
  // Drained.
  eq(buf.takeDue(1011, HOUR), null, 'nothing to send again immediately');
  // A new event within the hour accumulates but does not flush.
  buf.record('post-3', ['spam'], 1100, 120);
  eq(buf.takeDue(1100, HOUR), null, 'within the interval, nothing flushes');
  eq(buf.takeDue(1010 + HOUR, HOUR),
    { entries: [{ category: 'spam', count: 1 }], offsetMinutes: 120 },
    'the delta flushes once a full interval has passed');
});

Deno.test('notify buffer — the dedup set resets at the local day boundary', () => {
  const buf = new NotifyBufferState();
  buf.record('post-1', ['spam'], 1000, 0);
  // The same item on the next local day is a fresh enforcement.
  eq(buf.record('post-1', ['spam'], 1000 + 86_400, 0), true, 'next local day counts again');
  eq(buf.takeDue(1_000_000, HOUR)?.entries, [{ category: 'spam', count: 2 }], 'both counted');
});

// The regression this module's split exists for. The reset is
// driven by `actorScope.ts` on any identity change; these pin what it must drop.
Deno.test('notify buffer — an actor change drops the outgoing ward\'s pending counts', () => {
  const buf = new NotifyBufferState();
  buf.record('post-1', ['spam'], 1000, 120);

  buf.reset();

  // A Notify report carries no identity of its own, so counts that survive are
  // attributed to whoever is signed in when they drain.
  eq(buf.takeDue(1_000_000, HOUR), null,
    "the outgoing ward's pending counts survived the actor change");
});

Deno.test('notify buffer — an actor change drops the dedup set, so the incoming ward is not under-reported', () => {
  const buf = new NotifyBufferState();
  buf.record('post-1', ['spam'], 1000, 120);

  buf.reset();

  // Without this the incoming ward's first enforcement on an item the OUTGOING
  // ward already saw goes silently uncounted.
  eq(buf.record('post-1', ['spam'], 1000, 120), true,
    "the incoming ward's first enforcement on post-1 must count");
  eq(buf.takeDue(1000, HOUR)?.entries, [{ category: 'spam', count: 1 }], 'and it reports');
});

Deno.test('notify buffer — an actor change clears the flush clock, so the incoming ward reports eagerly', () => {
  const buf = new NotifyBufferState();
  buf.record('post-1', ['spam'], 1000, 120);
  buf.takeDue(1000, HOUR); // the outgoing ward's flush sets the clock

  buf.reset();

  // Carrying `lastFlush` across the switch would silence the incoming ward's
  // first report for up to a full interval.
  buf.record('post-2', ['spam'], 1050, 120);
  eq(buf.takeDue(1050, HOUR)?.entries, [{ category: 'spam', count: 1 }],
    "the incoming ward's first report must be eager");
});
