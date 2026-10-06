// Guards writes to the Recovery-kit section's shared `error-message` slot
// (`recoveryError` in `+page.svelte`) so a stolen-identity succession's
// persist-failure message — the ONLY surviving copy of the successor's new
// identity secret — cannot be silently clobbered by any other write to that
// shared, many-writer slot (`docs/goal/ui/settings.md` § Recovery kit → *The
// persist-failure message survives the page*, ratified 2026-09-14, all apps).
//
// Mirrors linux's `PENDING_STOLEN_FAILED_MESSAGE` / `render_account_error_label`
// (`apps/fauna-linux/src/settings/mod.rs`, ) and apple's
// `stolenFailedMessagePending` / `setErrorText` (`RecoveryKitVM.swift`,
// ). Web has no class instance to hang a guarded setter
// off of — `recoveryError` is a page-level `$state` — so this is a pure decision
// object instead: every writer passes its current value and its candidate
// through `write()` and assigns the result. Kept out of the `.svelte` file (and
// injected rather than reading Svelte state directly) so it is unit-testable
// under `just web-unit-test` — the Rust/Swift equivalents have their own native
// test runners; web's only unit lane is Deno over `$lib`
// (`recovery-error-guard.test.ts`).

export class RecoveryErrorGuard {
  #pending = false;

  /** True while a persist-failure message is parked and must not be clobbered.
   *  Not private, for the same testability reason as linux's/apple's own
   *  pending flags: a test seeds and reads it directly rather than driving a
   *  whole ceremony through a real API client. */
  get pending(): boolean {
    return this.#pending;
  }

  /**
   * The guarded write path every ORDINARY `recoveryError` writer must call —
   * every button click handler and every async repaint hook, not only the
   * ones that show validation errors (rule A: one `error-message` per page).
   * Returns `current` unchanged while a persist-failure message is pending,
   * dropping the write rather than clobbering the only surviving copy of the
   * successor's key; returns `next` otherwise.
   */
  write(current: string, next: string): string {
    return this.#pending ? current : next;
  }

  /**
   * Park a persist-failure message: it wins over every other write until
   * `discharge()` runs. Returns the message unconditionally — this is the ONE
   * write to the slot that bypasses `write()`, because at this instant the
   * message IS the thing becoming pending, and `write()`'s guard would refuse
   * its own display.
   */
  park(message: string): string {
    this.#pending = true;
    return message;
  }

  /**
   * Discharge a still-pending message: the user left the Account sub-page, or
   * the signed-in identity changed — either way they had the whole visit to
   * read or copy the successor's key. A no-op when nothing is pending. Only
   * un-guards future writes; callers that also want the slot's CONTENT
   * cleared (identity change, matching apple's `resetForIdentityChange`) call
   * `discharge()` first and then write `''` directly — discharging always
   * before clearing keeps that second write from being dropped by its own
   * guard.
   */
  discharge(): void {
    this.#pending = false;
  }
}
