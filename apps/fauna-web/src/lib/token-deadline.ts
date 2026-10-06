/** The SPA's bearer-deadline anchor — the TypeScript twin of Rust's
 *  `fauna_protocol::auth::deadline_on_own_clock` (`docs/goal/behavior/login.md`
 *  § Token lifetime on the client's clock).
 *
 *  A mint reply's `expires_at` is on the **nest's** clock, but every deadline
 *  the SPA derives from it — `getAuthToken`'s spend rule, the own-session-id
 *  pruning — is compared against the device's own clock (`$lib/api`'s
 *  `clientNowSecs()`). On a device whose clock is hours
 *  wrong the two disagree by hours: ahead re-mints on every request, behind
 *  serves a token the nest stopped honouring. So the reply also carries
 *  `expires_in` (seconds from the reply) and the SPA anchors it at receipt:
 *  `now + expires_in`. Nothing about the device's clock is corrected or
 *  reported. `expires_in` is required — every nest sends it (the older-nest
 *  fallback to `expires_at` retired 2026-09-24 with the compat-remnant sweep);
 *  `expiresAt` is kept as the argument the Rust twin also takes, for a caller
 *  whose clock cannot be read (the SPA's always can).
 *
 *  A module of its own for the reason `own-session-ids.ts` is: `api.ts` pulls
 *  in wasm at import time, so nothing in it can be reached by `deno test`. */
export function deadlineOnOwnClock(nowSecs: number, expiresIn: number, _expiresAt: number): number {
  return nowSecs + expiresIn;
}

// The `now` it is handed is `$lib/api`'s `clientNowSecs()`, read right as the
// reply arrives — the SPA's one client clock (which in a test build carries the
// wrong-clock witness's offset), so no mint site reads `Date.now()` itself.
