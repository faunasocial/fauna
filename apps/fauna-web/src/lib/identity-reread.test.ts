// Deno tests for the identity store's registry re-read merge. Run via:
//
//     deno test apps/fauna-web/src/lib/identity-reread.test.ts
//
// The end-to-end witnesses are the web succession closing-act tests
// (`test_identity_succession_ceremony.py`, `test_identity_succession_aftermath.py`),
// which timed out in `await_session_actor` whenever a route's `init()` re-read
// the registry after the verified refresh had landed; these pin the rule itself.

import type { Identity } from './types.ts';
import { keepVerifiedSession } from './identity-reread.ts';

const A: Identity = { secretHex: 'aa', actorId: 'actor-a', handle: 'alice' };
const B: Identity = { secretHex: 'bb', actorId: 'actor-b' };

Deno.test('a re-read of the SAME identity keeps its verified session', () => {
  const got = keepVerifiedSession({ ...A, registered: true }, { ...A });
  if (got?.registered !== true) throw new Error(`registered dropped: ${JSON.stringify(got)}`);
});

Deno.test('the re-read wins every other field', () => {
  const got = keepVerifiedSession({ ...A, registered: true }, { ...A, handle: 'alice2' });
  if (got?.handle !== 'alice2') throw new Error(`stale handle kept: ${JSON.stringify(got)}`);
});

Deno.test('a DIFFERENT identity never inherits the verified session', () => {
  const got = keepVerifiedSession({ ...A, registered: true }, { ...B });
  if (got?.registered) throw new Error(`verification smudged onto B: ${JSON.stringify(got)}`);
});

Deno.test('an unverified current identity grants nothing', () => {
  const got = keepVerifiedSession({ ...A }, { ...A });
  if (got?.registered) throw new Error(`registered invented: ${JSON.stringify(got)}`);
});

Deno.test('null on either side passes the incoming value through', () => {
  if (keepVerifiedSession(null, { ...A })?.registered) throw new Error('registered from nothing');
  if (keepVerifiedSession({ ...A, registered: true }, null) !== null) {
    throw new Error('a re-read finding no identity must clear the store');
  }
});
