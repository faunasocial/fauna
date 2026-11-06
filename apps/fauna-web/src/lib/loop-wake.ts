// What ends a slow background loop's wait early — the web twin of tui's and
// linux's `loop_wake::LoopWake`, the seam behind the e2e agent's `run_now`-style
// pokes (convention 14), so a witness drives the production loop's own next
// pass instead of waiting out its clock.
//
// Pure and dependency-free. A loop reaches `wake()` only through a caller that
// the e2e build alone installs (convention 15): the certificate auto-renew loop
// (`#lib/cert-auto-renew`) races this wait only under `__FAUNA_E2E_AUTOMATION__`,
// which a production `vite build` constant-folds away, so a shipped tab's loop
// runs on its clock alone.

/** A poke that ends one loop's current wait. */
export class LoopWake {
  /** A wake that landed while no wait was pending — kept for the next one, so a
   *  test never loses its poke to a busy loop (tokio `Notify`'s stored permit). */
  private pending = false;
  private release: (() => void) | null = null;

  /** End the loop's current wait now, or the very next one if none is pending.
   *  Several pokes before a wait are one permit. */
  wake(): void {
    if (this.release) {
      const release = this.release;
      this.release = null;
      release();
    } else {
      this.pending = true;
    }
  }

  /** Sleep `ms` — or until `wake()` ends the wait first. */
  sleep(ms: number): Promise<void> {
    if (this.pending) {
      this.pending = false;
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      const release = () => {
        clearTimeout(timer);
        resolve();
      };
      const timer = setTimeout(() => {
        if (this.release === release) this.release = null;
        resolve();
      }, ms);
      this.release = release;
    });
  }
}
