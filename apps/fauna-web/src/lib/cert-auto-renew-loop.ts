// web's certificate auto-renew tick (`tls-certificates.md` § C.3, *Any synced
// admin device renews*: "a desktop or web client covers a mobile-only gap") —
// the loop tui's `admin::spawn_auto_renew_cadence` and linux's cadence run.
//
// **Nothing about the tick is decided here.** The cadence, the refresh-then-ask
// order, the skip-if-empty rule, the per-domain non-fatal rule and the trailing
// health re-read are the shared `fauna_client_dns` (`auto_renew_poll_secs`,
// `DnsManagementMachine::auto_renew_scan` / `::auto_renew_issue`, over wasm). The
// loop owns only what is the app's: the timer, the `target_nest_id` resolution
// (linked-nests state, D7 — the nest the connection is bound to, never the
// nest's own claim), the log sink and telling an open page to re-read.
//
// Pure: every effect is a dependency, so the deno test drives it with fakes;
// `#lib/cert-auto-renew` wires the real ones.

/** What one `auto_renew_issue` pass did (`fauna_client_dns::AutoRenewPass`). */
export interface AutoRenewPass {
  issued: string[];
  failed: { domain: string; error: string }[];
}

export interface CertAutoRenewDeps {
  /** The shared cadence in seconds (`auto_renew_poll_secs`). */
  pollSecs: () => number;
  /** The wait — a raceable `LoopWake.sleep` in an e2e build, the clock otherwise. */
  sleep: (ms: number) => Promise<void>;
  /** True once the loop's owner has gone (identity change, sign-out). */
  stopped: () => boolean;
  /** `auto_renew_scan`: the domains due now, `[]` for none. */
  scan: () => Promise<string[]>;
  /** The connection-bound nest id (`LinkedNestsMachine::bound_nest_id`). */
  resolveTargetNestId: () => Promise<number[]>;
  /** `auto_renew_issue`. */
  issue: (domains: string[], targetNestId: number[]) => Promise<AutoRenewPass>;
  warn: (line: string) => void;
  /** A pass issued or tried something: an open `admin-dns` page re-reads. */
  shipped: () => void;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Run the tick until `stopped()`. The first pass fires after a full period, as
 *  on every app: a short-lived session never triggers a real CA order. */
export async function runCertAutoRenewLoop(deps: CertAutoRenewDeps): Promise<void> {
  for (;;) {
    await deps.sleep(deps.pollSecs() * 1000);
    if (deps.stopped()) return;
    try {
      const due = await deps.scan();
      // Nothing due ⇒ resolve no nest id, issue nothing, ship nothing.
      if (due.length === 0) continue;
      let targetNestId: number[];
      try {
        targetNestId = await deps.resolveTargetNestId();
      } catch (e) {
        deps.warn(`[admin] auto-renew cadence: resolve target nest id: ${message(e)}`);
        continue;
      }
      if (deps.stopped()) return;
      const pass = await deps.issue(due, targetNestId);
      for (const f of pass.failed) {
        deps.warn(`[admin] auto-renew cadence: IssueCert ${f.domain}: ${f.error}`);
      }
      deps.shipped();
    } catch (e) {
      // Best-effort by construction: one bad pass never stops the loop.
      deps.warn(`[admin] auto-renew cadence: pass failed: ${message(e)}`);
    }
  }
}
