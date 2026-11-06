// Deno tests for the background silent sign-in's retry decision. Run via:
//
//     deno test apps/fauna-web/src/lib/silent-sign-in-retry.test.ts
//
// The end-to-end witness (a real refused connect, then the nest coming back)
// is `tests/e2e-unified/tests/test_silent_sign_in_retry_web.py`; these pin the
// two rules that test cannot reach cheaply: WHICH failures retry, and that the
// schedule is capped rather than growing without bound.

import {
  IdentitySupersededError,
  NestIdentityChangedError,
  NestOutdatedError,
  TransientAuthError,
} from './auth-errors.ts';
import {
  LOST_NEST_OFFER_ATTEMPT,
  SILENT_SIGN_IN_RETRY_MS,
  lostNestOfferDue,
  silentSignInRetryDelay,
} from './silent-sign-in-retry.ts';

Deno.test('a transient failure is retried on the schedule, first interval first', () => {
  const err = new TransientAuthError('transient: anonymous connect: refused');
  for (let i = 0; i < SILENT_SIGN_IN_RETRY_MS.length; i++) {
    const got = silentSignInRetryDelay(err, i);
    if (got !== SILENT_SIGN_IN_RETRY_MS[i]) {
      throw new Error(`attempt ${i}: expected ${SILENT_SIGN_IN_RETRY_MS[i]}, got ${got}`);
    }
  }
});

Deno.test('the schedule is capped: every later attempt waits the last interval', () => {
  const err = new TransientAuthError('transient: timeout');
  const cap = SILENT_SIGN_IN_RETRY_MS[SILENT_SIGN_IN_RETRY_MS.length - 1];
  for (const attempt of [SILENT_SIGN_IN_RETRY_MS.length, 50, 10_000]) {
    const got = silentSignInRetryDelay(err, attempt);
    if (got !== cap) throw new Error(`attempt ${attempt}: expected the cap ${cap}, got ${got}`);
  }
});

Deno.test('the lost-nest question is asked exactly once, after a blip has had its chance', () => {
  const asked = Array.from({ length: 50 }, (_, i) => i).filter(lostNestOfferDue);
  if (asked.length !== 1 || asked[0] !== LOST_NEST_OFFER_ATTEMPT) {
    throw new Error(`expected one ask at ${LOST_NEST_OFFER_ATTEMPT}, got ${asked}`);
  }
  if (LOST_NEST_OFFER_ATTEMPT < 1) {
    throw new Error('the first failure is a blip until proven otherwise');
  }
  if (LOST_NEST_OFFER_ATTEMPT >= SILENT_SIGN_IN_RETRY_MS.length) {
    throw new Error('the ask must land before the schedule reaches its long cap');
  }
});

Deno.test('the opaque Error bucket is NOT retried — a flattened verdict must stay visible', () => {
  if (silentSignInRetryDelay(new Error('codec: unexpected end of input'), 0) !== null) {
    throw new Error('a plain Error must end the refresh, not enter the retry loop');
  }
});

Deno.test('terminal verdicts and non-errors are never retried', () => {
  const cases: unknown[] = [
    new NestOutdatedError('update required'),
    new NestIdentityChangedError('https://nest.example', 'aa', 'bb'),
    new IdentitySupersededError('cc'),
    'transient: a bare string is not the typed class',
    null,
    undefined,
  ];
  for (const c of cases) {
    if (silentSignInRetryDelay(c, 0) !== null) {
      throw new Error(`expected no retry for ${String(c)}`);
    }
  }
});
