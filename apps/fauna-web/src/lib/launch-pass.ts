// When a launch pass that rests on the account store runs — web's leg of the
// rule `fauna_client_folders::launch_resume` owns for the native seats.
//
// The conversations manager's build runs launch passes that read the folder-key
// custody, and that custody rests in this tab's account runtime, which starts
// beside the build and is never awaited by it (`$lib/account-runtime`: the
// store opening must not hold the conversations page back). So such a pass has
// two edges — the build reaching it, and the runtime having started — and runs
// at whichever comes last:
//
// - the runtime has started → the pass runs in line, and the build waits for
//   it as it always has;
// - the runtime is still starting → the caller gets control back at once and
//   the pass runs when the start settles, if it settled started and the build
//   is still this tab's. A start that failed skips the pass and says so; the
//   next launch retries.
//
// Before this the pass ran over a custody that answered "not running", was
// logged and swallowed, and the recovery waited for a launch that happened to
// win the race.

export interface LaterEdge {
  /** Has this tab's account runtime started (custody readable now)? */
  started: () => boolean;
  /** Resolves once the in-flight runtime start has settled; never rejects. */
  settled: () => Promise<void>;
  /** Is the build this pass belongs to still the tab's — no actor switch, not
   *  abandoned? Checked again after the wait: the pass writes MLS state. */
  stillWanted: () => boolean;
  /** The pass. Owns its own failure reporting; must not reject. */
  pass: () => Promise<void>;
  /** The pass was not run because the runtime did not start. */
  skipped: (why: string) => void;
}

/** Run `edge.pass` at the later of its two edges. Resolves when an in-line
 *  pass has finished, or at once when the pass was left to the runtime's edge. */
export async function runAtLaterEdge(edge: LaterEdge): Promise<void> {
  if (edge.started()) {
    await edge.pass();
    return;
  }
  void edge.settled().then(() => {
    if (!edge.stillWanted()) return;
    if (!edge.started()) {
      edge.skipped('the account runtime did not start; the next launch retries');
      return;
    }
    return edge.pass();
  });
}
