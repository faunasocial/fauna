// Deno tests for the silent-challenge error classifier. Run via:
//
//     deno test apps/fauna-web/src/lib/auth-errors.test.ts
//
// `classifyChallengeError` is the web sibling of the shared
// `RpcError::action()` classifier (libs/fauna-protocol): the wasm
// `map_silent_err` prefixes a transient transport fault with `"transient:"`
// and a degraded-nest `fauna.nest.outdated` with `"outdated:"`; everything
// else is opaque. The launch screen routes the three to retry / non-retry
// "update required" / generic-unreachable respectively
// (version-compatibility.md Dim 4).

import {
  AccountLockedError,
  classifyChallengeError,
  IdentitySupersededError,
  NestOutdatedError,
  SignInRefusedError,
  TransientAuthError,
} from './auth-errors.ts';

Deno.test('an "outdated:" rejection → NestOutdatedError carrying the localized message (prefix stripped)', () => {
  const localized =
    'This nest is running an outdated version and must be updated before you can connect.';
  const err = classifyChallengeError(`outdated: ${localized}`);
  if (!(err instanceof NestOutdatedError)) {
    throw new Error(`expected NestOutdatedError, got ${err.constructor.name}`);
  }
  if (err.message !== localized) {
    throw new Error(`expected the localized message without the prefix, got: ${err.message}`);
  }
});

Deno.test('a "transient:" rejection → TransientAuthError (full string preserved)', () => {
  const err = classifyChallengeError('transient: anonymous connect https://nest.example: refused');
  if (!(err instanceof TransientAuthError)) {
    throw new Error(`expected TransientAuthError, got ${err.constructor.name}`);
  }
});

Deno.test('any other rejection → a plain Error (treated as unreachable)', () => {
  const err = classifyChallengeError('codec: unexpected end of input');
  if (err instanceof TransientAuthError || err instanceof NestOutdatedError) {
    throw new Error(`expected a plain Error, got ${err.constructor.name}`);
  }
  if (err.message !== 'codec: unexpected end of input') {
    throw new Error(`expected the raw message preserved, got: ${err.message}`);
  }
});

Deno.test('a thrown Error object (not a string) is classified by its .message', () => {
  const err = classifyChallengeError(new Error('outdated: nest too old'));
  if (!(err instanceof NestOutdatedError)) {
    throw new Error(`expected NestOutdatedError from an Error.message, got ${err.constructor.name}`);
  }
  if (err.message !== 'nest too old') {
    throw new Error(`unexpected message: ${err.message}`);
  }
});

// ── The succeeded-identity verdict ──────────────────────────────────────────
//
// Web's analogue of the four FFI apps' `FfiError::IdentitySuperseded`. Until
// 2026-09-01 `map_silent_err` had no arm for it, so a succeeded device's refusal
// arrived here as an opaque string, fell to the plain-`Error` case below, and
// `store.ts`'s catch logged and swallowed it — leaving the user on the feed with
// a session the nest had already refused. These pin the classification that
// makes the escalation reachable at all.

Deno.test('a "superseded:" rejection → IdentitySupersededError carrying the claimed successor', () => {
  const successor = 'ab'.repeat(32);
  const err = classifyChallengeError(`superseded: ${successor}`);
  if (!(err instanceof IdentitySupersededError)) {
    throw new Error(`expected IdentitySupersededError, got ${err.constructor.name}`);
  }
  if (err.claimedSuccessor !== successor) {
    throw new Error(`expected the successor carried, got ${err.claimedSuccessor}`);
  }
});

Deno.test('the successor is CARRIED, not put in the message shown to the user', () => {
  // The nest is enforcer and distributor, never authorizer: the claim rides the
  // error so an admin's log and the escalation can see it, and the surface names
  // a successor only once the registration chain proves one. A message that
  // embedded the hex would be one careless render away from presenting the
  // nest's claim as fact.
  const successor = 'cd'.repeat(32);
  const err = classifyChallengeError(`superseded: ${successor}`);
  if (err.message.includes(successor)) {
    throw new Error(`the claimed successor must not be in the displayed message: ${err.message}`);
  }
});

Deno.test('a thrown IdentitySupersededError survives re-classification unchanged', () => {
  // The downgrade trap `escalateIfTerminalAuthVerdict` exists to avoid: an
  // already-typed verdict run back through the classifier reads `.message`,
  // which no longer carries the wire prefix. The adapter must check `instanceof`
  // FIRST — this pins what happens if it ever stops doing so.
  const typed = new IdentitySupersededError('ef'.repeat(32));
  const reclassified = classifyChallengeError(typed);
  if (reclassified instanceof IdentitySupersededError) {
    throw new Error(
      'a typed verdict now survives re-classification — the adapter\'s instanceof ' +
        'check is no longer load-bearing, so this test should be rewritten rather ' +
        'than deleted',
    );
  }
});

// ── The sign-in refusal ─────────────────────────────────────────────────────
//
// `getAuthToken`'s `fauna.auth.not_registered`. Its `code` is a contract with
// the wasm WS client, whose token provider reads it off the rejection to stop
// the reconnect loop as a refusal instead of backing off into a retry loop
// (`fauna_rpc_wasm`'s `is_sign_in_refusal`) — so the field, not the message,
// is what must hold.

Deno.test('SignInRefusedError carries the wire code the wasm token provider keys on', () => {
  const err = new SignInRefusedError('https://nest.example');
  if (Reflect.get(err, 'code') !== 'fauna.auth.not_registered') {
    throw new Error(`the wire code must be a readable property (wasm reads it by Reflect.get), got ${err.code}`);
  }
  if (err.origin !== 'https://nest.example') {
    throw new Error(`expected the refusing origin carried, got ${err.origin}`);
  }
});

// ── The locked-account verdict ──────────────────────────────────────────────
//
// `fauna.auth.verify` refuses a locked account (`login.md` § Silent Challenge).
// The wasm `map_silent_err` prefixes it `locked: <unix-secs>`; it must not fall
// into the opaque bucket, where a retry loop would re-earn the refusal.

Deno.test('a "locked:" rejection → AccountLockedError carrying the unlock time', () => {
  const err = classifyChallengeError('locked: 1700086400');
  if (!(err instanceof AccountLockedError)) {
    throw new Error(`expected AccountLockedError, got ${err.constructor.name}`);
  }
  if (err.lockedUntilSecs !== 1700086400) {
    throw new Error(`expected the unlock time carried, got ${err.lockedUntilSecs}`);
  }
});

Deno.test('a "locked:" rejection with no readable time is not invented into one', () => {
  const err = classifyChallengeError('locked: soon');
  if (err instanceof AccountLockedError) {
    throw new Error('an unreadable unlock time must fall through to the opaque bucket');
  }
});
