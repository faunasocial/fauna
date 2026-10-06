import { ReceivePump, type ReceivePumpOptions } from './receive-pump.ts';

// tier_1 pins for the web receive pump (`receive-pump.ts` owns the reasoning).
// The ceiling here is tiny so a stall arrives at once; every positive wait still
// has a generous budget of its own, so a loaded box can slow these tests down
// but never flip their verdict (convention 14).

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

/** Far above any non-pathological delay; a green run spends none of it. */
const BUDGET_MS = 30_000;

/** `promise`, or a failure naming `what` once the budget is spent. */
async function withinBudget<T>(promise: Promise<T>, what: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`timed out after ${BUDGET_MS} ms waiting for ${what}`)),
      BUDGET_MS,
    );
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer);
  }
}

/** A pump over the given rails, with a latch per stall edge. */
function pumpWith(conv: ReceivePumpOptions['conv'], ceilingMs: number) {
  const edges: boolean[] = [];
  let onStall: () => void = () => {};
  let onClear: () => void = () => {};
  const stalled = new Promise<void>((resolve) => (onStall = resolve));
  const cleared = new Promise<void>((resolve) => (onClear = resolve));
  const pump = new ReceivePump({
    conv,
    mail: () => Promise.resolve(),
    ceilingMs,
    onStalledChange: (s) => {
      edges.push(s);
      if (s) onStall();
      else onClear();
    },
  });
  return { pump, edges, stalled, cleared };
}

Deno.test('receive pump — a pass that never settles is reported stalled, and no second pass starts over it', async () => {
  // The web outage's shape: a panicking wasm task's promise never settles.
  let passes = 0;
  const { pump, stalled } = pumpWith(() => {
    passes += 1;
    return new Promise<void>(() => {});
  }, 10);

  void pump.cycle();
  await withinBudget(stalled, 'the never-settling pass to be reported stalled');
  // A FIFO macrotask sentinel, not a delay: every microtask the pump queued on
  // the stall edge — including the continuation that would clear `busy` if the
  // pump let go of the dead pass — has run before this resolves. Without it the
  // asserts below read the pump before a re-arming pump could have re-armed.
  await new Promise<void>((resolve) => setTimeout(resolve, 0));

  eq(pump.exit(), 'stalled', 'the stalled pump publishes its exit');
  eq(pump.isBusy(), true, 'the pump stays parked on the dead pass');
  // Every arm that could re-arm it: the busy check returns synchronously, so
  // the pass count is already final when these calls return.
  pump.wakeConv();
  pump.wakeMail();
  void pump.cycle();
  eq(passes, 1, 'a stalled pump must never start another pass over the poisoned side');
  eq(pump.cycles(), [1, 0], 'the dead cycle began and never completed');
});

Deno.test('receive pump — a pass that settles after the ceiling clears the stall and the pump carries on', async () => {
  let release: () => void = () => {};
  let passes = 0;
  const { pump, edges, stalled, cleared } = pumpWith(() => {
    passes += 1;
    return passes === 1 ? new Promise<void>((resolve) => (release = resolve)) : Promise.resolve();
  }, 10);

  const first = pump.cycle();
  await withinBudget(stalled, 'the slow pass to be reported stalled');
  release();
  await withinBudget(cleared, 'the stall to clear once the slow pass settled');
  await withinBudget(first, 'the first cycle to return');

  eq(pump.exit(), null, 'a pass that settled was slow, not dead');
  eq(pump.cycles(), [1, 1], 'the slow cycle completed');
  await withinBudget(pump.cycle(), 'a fresh cycle after the recovery');
  eq(passes, 2, 'the recovered pump runs passes again');
  eq(pump.cycles(), [2, 2], 'and counts them');
  eq(edges, [true, false], 'exactly one stall edge and one clear edge');
});

Deno.test('receive pump — a pass inside the ceiling reports nothing and leaves no timer behind', async () => {
  // Deno's op sanitizer fails this test if the ceiling timer outlives the pass,
  // which is what pins the `clearTimeout`.
  const { pump, edges } = pumpWith(() => Promise.resolve(), 60_000);
  await withinBudget(pump.cycle(), 'a healthy cycle');
  eq(edges, [], 'a healthy pass never touches the stall state');
  eq(pump.exit(), null, 'a healthy pump publishes no exit');
  eq(pump.cycles(), [1, 1], 'one counted cycle');
});

Deno.test('receive pump — a wake mid-pass coalesces into the running pump instead of overlapping it', async () => {
  let inFlight = 0;
  let maxInFlight = 0;
  let passes = 0;
  let releaseFirst: () => void = () => {};
  const { pump } = pumpWith(() => {
    passes += 1;
    inFlight += 1;
    maxInFlight = Math.max(maxInFlight, inFlight);
    const done = passes === 1 ? new Promise<void>((resolve) => (releaseFirst = resolve)) : Promise.resolve();
    return done.then(() => {
      inFlight -= 1;
    });
  }, 60_000);

  const first = pump.cycle();
  // A push and a poke land while the first pass is in flight.
  pump.wakeConv();
  void pump.cycle();
  releaseFirst();
  await withinBudget(first, 'the running pump to drain the coalesced wakes');

  eq(maxInFlight, 1, 'no two passes ever overlapped');
  eq(passes, 2, 'the coalesced wakes produced exactly one further pass');
  eq(pump.cycles(), [2, 2], 'the mid-pass poke got its own counted cycle');
});
