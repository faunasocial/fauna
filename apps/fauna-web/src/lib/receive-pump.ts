// The web receive pump — the web twin of the shared loop's `tokio::select!`.
//
// `ConversationsSession::start_receive_loop` (`libs/fauna-conversations/src/
// session.rs`) drives the native apps' receive: ONE task selecting a 30s
// backstop ticker against a push arm, so no two receive passes ever overlap on
// the one MLS engine. That loop is `cfg(not(target_arch = "wasm32"))` — the
// `Rc`-based wasm RPC client is `!Send`, so web cannot run it and mirrors its
// arms in `$lib/conversations`, which owns the rails and the arms. This module
// owns the part those arms share: serialization, cycle counting, and the pass
// ceiling. It holds no wasm, so all three are pinned by a plain `deno test`
// (`receive-pump.test.ts`).
//
// **Serialization.** Natively, ticker and push are two arms of one task and can
// never run concurrently. On web they are independent JS callers (a
// `setTimeout` chain and a WS `onmessage` callback) against a
// `WasmConversationsManager` whose interior state is `RefCell` and whose MLS
// engine is single-threaded. Letting them overlap would be a real bug, not a
// style problem:
//
//   * a Welcome ingested twice **double-spends the MLS init key** — precisely the
//     hazard linux avoids by no-op'ing `PushEvent::Welcome` in its central
//     dispatch (`app.rs`) and letting only the receive loop ingest it; and
//   * a re-entrant `borrow_mut()` on a live `RefCell` borrow **panics**, taking
//     the SPA down.
//
// So every receive pass — ticker or push — funnels through one pump, which runs
// at most one pass at a time and *coalesces* the rest: a wake that arrives
// mid-pass sets its rail's flag and the running pump picks it up on its next
// turn. That matches the native semantics exactly, where a burst of channel
// pushes collapses into one `poll_bound` sweep (the per-channel cursor dedups —
// "an arrival or a lag both resolve to 'poll now'").
//
// **The ceiling — why a pass is bounded at all.** wasm has no unwinding
// (`libs/fauna-wasm-panic-hook`): a panic inside a pass aborts its
// `future_to_promise` task and the promise NEVER settles. An unbounded pump then
// sits busy forever, every later wake returns at the busy check, and both rails
// are dead for the tab with no observable but a console line — which is exactly
// how every real web session's receive rail stayed dead from 2026-09-09 to
// 2026-09-12 (`conversation-rooms.md` § Implementation status today). So each
// rail's pass races a named ceiling; a pass that outruns it marks the pump
// STALLED, which the conversations page shows on `error-message`
// (`ui/conversations.md` § Errors & edge cases) and the e2e state publishes as
// `conv_receive_cycles.exit = "stalled"` (`fauna_e2e_agent::
// CONV_RECEIVE_CYCLES_KEY`).
//
// A stalled pump is **reported, never re-armed**: the aborted task may have left
// the wasm side poisoned (the sweep cell, the channel lock held across its
// await), so no new pass is started over it — the pump simply stays parked on
// the unsettled pass, which also parks the backstop ticker awaiting it. The one
// way out short of a reload is the pass settling after all: that proves it was
// slow rather than dead (an aborted task cannot settle), so the stall clears and
// the pump carries on. That is also what keeps a false stall — a timer that
// fired on wake from sleep, having measured the sleep rather than the pass —
// from standing: the RPC in flight settles moments later and the banner goes.

/** How long one rail's pass may run before the pump is reported stalled —
 *  convention 14's named generous ceiling
 *  (`e2e-latency-independent-assertions.md` § The convention — convention 14),
 *  sized far above any non-pathological pass rather than tuned to observed
 *  ones: a healthy pass is seconds even on a loaded box, and a pass that fails
 *  transiently (disconnect, backend not yet active) fails fast and settles. A
 *  generous ceiling costs a healthy tab nothing — only a pass that never
 *  settles ever spends it. */
export const RECEIVE_PASS_CEILING_MS = 10 * 60 * 1000;

/** One rail's pass. By contract it never rejects (each rail owns its error
 *  handling); a rejection is treated as settled rather than trusted to. */
export type RailPass = () => Promise<void>;

export interface ReceivePumpOptions {
  /** The FaunaMls rail's pass. */
  conv: RailPass;
  /** The SMTP mail rail's pass. */
  mail: RailPass;
  /** [`RECEIVE_PASS_CEILING_MS`] in production; tests pass a small one. */
  ceilingMs: number;
  /** Called on each stalled ↔ running edge. */
  onStalledChange: (stalled: boolean) => void;
}

export class ReceivePump {
  private readonly opts: ReceivePumpOptions;
  /** True while a pass runs — the pump's mutual-exclusion flag. */
  private busy = false;
  /** Coalescing wake flags: set by a push (or the ticker), consumed by `run`. */
  private wantConv = false;
  private wantMail = false;
  /** A full-cycle request — the ticker arm, the reconnect arm and the poke set
   *  it. Unlike `wantConv`/`wantMail` it is cleared only by a pass that
   *  *counts* itself, which is what makes a poke landing mid-pass produce a
   *  fresh cycle rather than being absorbed by the pass already running. */
  private wantCycle = false;
  /** The web twin of native's `ReceiveCycles` (`fauna_e2e_agent::
   *  CONV_RECEIVE_CYCLES_KEY` owns the contract): full receive cycles begun and
   *  finished. `started` is bumped before a cycle reads any rail and
   *  `completed` after both rails are quiet, so a consumer that read `started`
   *  before its trigger and waits for `completed` to pass it can never be
   *  released by a cycle that was already in flight. */
  private started = 0;
  private completed = 0;
  private stalled = false;

  constructor(opts: ReceivePumpOptions) {
    this.opts = opts;
  }

  /** `[started, completed]`. */
  cycles(): [number, number] {
    return [this.started, this.completed];
  }

  /** `'stalled'` while a pass has outrun the ceiling, else `null` — web's value
   *  for `conv_receive_cycles.exit`. */
  exit(): 'stalled' | null {
    return this.stalled ? 'stalled' : null;
  }

  /** Whether a pass is in flight. */
  isBusy(): boolean {
    return this.busy;
  }

  /** Wake the FaunaMls rail — a push arm. */
  wakeConv(): void {
    this.wantConv = true;
    void this.run();
  }

  /** Wake the SMTP mail rail — a push arm. */
  wakeMail(): void {
    this.wantMail = true;
    void this.run();
  }

  /** Request a full, counted cycle over both rails — the ticker, reconnect and
   *  poke arms. Resolves when this call's pump returns, or at once when a pump
   *  already running will pick the request up (that is the coalescing). */
  cycle(): Promise<void> {
    this.wantCycle = true;
    this.wantConv = true;
    this.wantMail = true;
    return this.run();
  }

  /** Run passes until both rails are quiet, one at a time. Re-entrant calls
   *  return immediately — synchronously, before any await — so a second pass
   *  can never start while one is in flight. Never throws. */
  private async run(): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    try {
      // The outer `do` exists for the cycle counters: a full-cycle request that
      // arrives while this pass is inside the rail loop keeps `wantCycle` set, so
      // it gets its own counted round here rather than waiting for the next tick.
      do {
        const counted = this.wantCycle;
        if (counted) {
          this.wantCycle = false;
          // Bump BEFORE any rail is read — the ordering the consumer's pigeonhole
          // rests on (native pins it in `ReceiveCycles`).
          this.started += 1;
        }
        while (this.wantConv || this.wantMail) {
          if (this.wantConv) {
            this.wantConv = false;
            await this.bounded(this.opts.conv);
          }
          if (this.wantMail) {
            this.wantMail = false;
            await this.bounded(this.opts.mail);
          }
        }
        if (counted) this.completed += 1;
      } while (this.wantCycle);
    } finally {
      this.busy = false;
    }
  }

  /** Await one pass under the ceiling. Past it, report the stall and keep
   *  awaiting the same pass — never start another (module comment). */
  private async bounded(pass: RailPass): Promise<void> {
    const settled = pass().then(
      () => true as const,
      () => true as const,
    );
    let timer: ReturnType<typeof setTimeout> | undefined;
    const ceiling = new Promise<false>((resolve) => {
      timer = setTimeout(() => resolve(false), this.opts.ceilingMs);
    });
    const inTime = await Promise.race([settled, ceiling]);
    clearTimeout(timer);
    if (inTime) return;
    this.setStalled(true);
    // An aborted wasm task never settles, so a dead pump parks here for good.
    await settled;
    // It settled after all: slow, not dead.
    this.setStalled(false);
  }

  private setStalled(stalled: boolean): void {
    if (this.stalled === stalled) return;
    this.stalled = stalled;
    this.opts.onStalledChange(stalled);
  }
}
