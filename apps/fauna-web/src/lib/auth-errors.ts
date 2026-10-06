//! Typed silent-challenge errors + their classifier.
//!
//! Extracted from `wasm.ts` so the pure classification logic is unit-testable
//! without pulling in the SvelteKit `$app/*` aliases or the wasm glue. Re-exported
//! from `$lib/wasm` for existing call sites.

/** A transient transport failure from `challengeVerify` (anonymous-WS connect /
 *  timeout / disconnect). The onboarding launch screen treats it as `transient`
 *  (Retry CTA), mirroring the old HTTP path's `TypeError`. */
export class TransientAuthError extends Error {}

/** A degraded-nest (`fauna.nest.outdated`) rejection from `challengeVerify`: the
 *  nest authoritatively reports it is running an outdated version, so the launch
 *  screen routes to a NON-retry "update required" surface (never the Retry CTA /
 *  wizard drop — retrying the same outdated nest is futile). The web sibling of
 *  the FFI's `FfiError::NestOutdated` and the launch machine's
 *  `Offline { transient: false }` (version-compatibility.md Dim 4). `.message` is
 *  the already-localized actionable text (the `"outdated:"` wire prefix stripped). */
export class NestOutdatedError extends Error {}

/** The nest's pinned deployment identity changed — or it can no longer prove the
 *  identity we previously pinned (`docs/goal/architecture/security.md`
 *  § Transport trust, the web-exempt note). The SSH `known_hosts` "REMOTE HOST
 *  IDENTIFICATION HAS CHANGED" case: the launch screen blocks auto-entry and shows
 *  the `nest-identity-changed-warning`, offering an explicit re-trust
 *  (`forgetNestIdentityPin` → re-TOFU) or "use a different nest". `seen` is the
 *  newly-presented identity hex, or `null` when the nest presented no valid
 *  binding this connect (a withdrawn proof / downgrade attempt). */
export class NestIdentityChangedError extends Error {
  readonly origin: string;
  readonly pinned: string;
  readonly seen: string | null;
  constructor(origin: string, pinned: string, seen: string | null) {
    super(`nest identity changed for ${origin || 'this nest'}`);
    this.name = 'NestIdentityChangedError';
    this.origin = origin;
    this.pinned = pinned;
    this.seen = seen;
  }
}

/** This identity was SUCCEEDED: the account belongs to a different keypair now
 *  and this one can never sign in again (`identity-succession.md` § Propagation
 *  → *Own device fleet*). Terminal and account-level, not transport — the one
 *  verdict on these paths where a retry is guaranteed futile *and* the remedy
 *  is a different screen, so it escalates rather than degrading in place.
 *
 *  `claimedSuccessor` is what the refusal NAMED, and it is deliberately not for
 *  display: the launch surface shows the claim-free wording and names a
 *  successor only once the registration chain proves one. It is carried so the
 *  escalation can log it for an admin, and because dropping it here would mean
 *  re-deriving it from a second refusal later. */
export class IdentitySupersededError extends Error {
  readonly claimedSuccessor: string;
  constructor(claimedSuccessor: string) {
    super('this identity was succeeded');
    this.name = 'IdentitySupersededError';
    this.claimedSuccessor = claimedSuccessor;
  }
}

/** The account is LOCKED OUT — `fauna.auth.account_locked` (`login.md` § Silent
 *  Challenge). Terminal until `lockedUntilSecs` (Unix seconds): a retry before
 *  then only re-earns the refusal, so it is its own class rather than the
 *  retryable `TransientAuthError` or the opaque unreachable bucket. */
export class AccountLockedError extends Error {
  readonly lockedUntilSecs: number;
  constructor(lockedUntilSecs: number) {
    super('this account is locked');
    this.name = 'AccountLockedError';
    this.lockedUntilSecs = lockedUntilSecs;
  }
}

/** The nest refused to sign this identity in — `fauna.auth.not_registered`,
 *  suspended or removed, deliberately indistinguishable (`login.md` § Errors).
 *  `getAuthToken` throws it when a bearer mint's silent challenge comes back
 *  `null`; on the home nest it is the mid-session sign-in refusal that ends the
 *  session (`onboarding.md` § App-launch routing, the previously-signed-in row)
 *  and escalates to the launch surface.
 *
 *  `code` is the wire code and is load-bearing: the wasm WS client's token
 *  provider reads it off the rejection to stop its reconnect loop as a refusal
 *  instead of backing off (`fauna_rpc_wasm`'s `is_sign_in_refusal`). */
export class SignInRefusedError extends Error {
  readonly code = 'fauna.auth.not_registered';
  readonly origin: string;
  constructor(origin: string) {
    super(`fauna.auth.not_registered: no account for this identity on ${origin}`);
    this.name = 'SignInRefusedError';
    this.origin = origin;
  }
}

/** Classify a `challengeVerify` rejection into the typed launch-routing error.
 *  Mirrors the shared `RpcError::action()` classifier (`libs/fauna-protocol`): the
 *  wasm `map_silent_err` prefixes a transient transport fault with `"transient:"`
 *  and a degraded-nest `fauna.nest.outdated` with `"outdated:"`; everything else is
 *  an opaque `Error` the launch screen treats as `unreachable`. The `"outdated:"`
 *  prefix is stripped so the resulting `.message` is the bare localized text the UI
 *  renders directly; `"transient:"` keeps its full string (the launch screen shows a
 *  fixed connectivity message, not the raw detail). Pure string logic — unit-tested. */
export function classifyChallengeError(raw: unknown): Error {
  const msg = typeof raw === 'string' ? raw : String((raw as { message?: string })?.message ?? raw);
  if (msg.startsWith('transient:')) return new TransientAuthError(msg);
  if (msg.startsWith('outdated:')) {
    return new NestOutdatedError(msg.slice('outdated:'.length).trimStart());
  }
  // `nest-identity-changed:<json>` from the wasm possession-pin check — the nest's
  // pinned identity changed or its proof was withdrawn. The JSON
  // carries `{ origin, pinned, seen }` (`seen` null on a withdrawn proof).
  const NEST_ID_PREFIX = 'nest-identity-changed:';
  if (msg.startsWith(NEST_ID_PREFIX)) {
    try {
      const { origin, pinned, seen } = JSON.parse(msg.slice(NEST_ID_PREFIX.length)) as {
        origin?: string;
        pinned?: string;
        seen?: string | null;
      };
      return new NestIdentityChangedError(origin ?? '', pinned ?? '', seen ?? null);
    } catch {
      return new NestIdentityChangedError('', '', null);
    }
  }
  // `superseded: <64-hex>` from the wasm `map_silent_err` superseded arm.
  const SUPERSEDED_PREFIX = 'superseded:';
  if (msg.startsWith(SUPERSEDED_PREFIX)) {
    return new IdentitySupersededError(msg.slice(SUPERSEDED_PREFIX.length).trim());
  }
  // `locked: <unix-secs>` from the wasm `map_silent_err` locked arm. A prefix
  // with an unreadable time is not invented into one — it falls through opaque.
  const LOCKED_PREFIX = 'locked:';
  if (msg.startsWith(LOCKED_PREFIX)) {
    const secs = Number(msg.slice(LOCKED_PREFIX.length).trim());
    if (Number.isInteger(secs) && secs >= 0) return new AccountLockedError(secs);
  }
  return new Error(msg);
}
