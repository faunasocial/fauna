/** The SPA's locked-account latch — the TypeScript twin of the native bearer
 *  mints' latch (`fauna_client::ws_challenge_bearer`, "a standing lock is
 *  answered without dialling"; `docs/goal/behavior/devices.md` § The locked
 *  state). A mint the nest refused `fauna.auth.account_locked` is answered from
 *  here until the unlock time, so a locked tab's reconnect loop stops asking
 *  the nest: nothing clears a lock before its time, and the launch machine owns
 *  the one refresh at the unlock time.
 *
 *  Keyed like `$lib/api`'s token cache (nest + identity). Memory only. Reads no
 *  clock: `now` (Unix **seconds**) is the caller's — `$lib/api`'s
 *  `clientNowSecs()`. A module of its own for the reason `own-session-ids.ts`
 *  is: `api.ts` loads wasm at import time, so `deno test` cannot reach it. */
export class LockLatch {
  #until = new Map<string, number>();

  /** Record a lock refusal for `key`, standing until `lockedUntil`. */
  latch(key: string, lockedUntil: number): void {
    this.#until.set(key, lockedUntil);
  }

  /** The unlock time of a lock still standing for `key` at `now`, else `null`
   *  (a lapsed latch is dropped). */
  standing(key: string, now: number): number | null {
    const until = this.#until.get(key);
    if (until === undefined) return null;
    if (now < until) return until;
    this.#until.delete(key);
    return null;
  }

  /** A mint succeeded: whatever was latched for `key` is over. */
  clear(key: string): void {
    this.#until.delete(key);
  }
}
