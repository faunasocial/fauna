// Deno tests for the succession closing act's decision. Run via:
//
//     just web-unit-test
//
// The mint itself is one wasm call and not worth a test. What is worth one is
// the failure arm: apple shipped this step twice with a mint that SPENT the
// obligation instead of re-arming it, and the goal doc records that as a
// cross-platform trap rather than an iOS one — the ceremony revokes every
// session of the account inside the nest's own transaction, so the successor's
// first mint races its own reconnect everywhere. macOS passed on timing luck.

import { dischargeOwedKit, type OwedKitPort } from './succession-kit.ts';

/** A port that records what happened, so each test asserts on the sequence
 *  rather than on one flag. `mint` is supplied per test. */
function spyPort(mint: () => Promise<string>, owed = true) {
  const calls: string[] = [];
  const port: OwedKitPort<string> = {
    claim: () => {
      calls.push('claim');
      return Promise.resolve(owed);
    },
    mint: () => {
      calls.push('mint');
      return mint();
    },
    show: (m) => {
      calls.push(`show:${m}`);
    },
    rearm: () => {
      calls.push('rearm');
      return Promise.resolve();
    },
    onError: (m) => {
      calls.push(`error:${m}`);
    },
  };
  return { port, calls };
}

Deno.test('an ordinary sign-in claims nothing and mints nothing', () => {
  const { port, calls } = spyPort(() => Promise.reject(new Error('must not run')), false);
  return dischargeOwedKit(port).then((shown) => {
    if (shown) throw new Error('nothing was owed, so nothing can have been shown');
    if (calls.join(',') !== 'claim') {
      throw new Error(`a lost claim must stop everything; got ${calls.join(',')}`);
    }
  });
});

Deno.test('a won claim mints and SHOWS — the property is that the user saw it', async () => {
  const { port, calls } = spyPort(() => Promise.resolve('fresh-secret'));
  const shown = await dischargeOwedKit(port);
  if (!shown) throw new Error('a successful mint must report the kit as shown');
  if (calls.join(',') !== 'claim,mint,show:fresh-secret') {
    throw new Error(`unexpected sequence: ${calls.join(',')}`);
  }
  if (calls.includes('rearm')) {
    throw new Error('a shown kit must not leave the obligation owed — it is discharged');
  }
});

Deno.test('a FAILED mint re-arms the obligation, never spends it', async () => {
  // The regression this exists for. An unshown mint leaves a kit nobody holds,
  // which the goal doc calls strictly worse than never-created; if the
  // obligation is also gone, nothing anywhere remembers to try again and the
  // user's only route back is the 30-day seed-alone window.
  const { port, calls } = spyPort(() => Promise.reject(new Error('bearer revoked')));
  const shown = await dischargeOwedKit(port);
  if (shown) throw new Error('a failed mint must not report a kit as shown');
  if (!calls.includes('rearm')) {
    throw new Error(`the obligation must be re-armed; got ${calls.join(',')}`);
  }
  if (calls.some((c) => c.startsWith('show:'))) {
    throw new Error('nothing may be shown when the mint failed');
  }
  if (calls.join(',') !== 'claim,mint,error:bearer revoked,rearm') {
    throw new Error(`unexpected sequence: ${calls.join(',')}`);
  }
});

Deno.test('the user is told why, and the re-arm still happens', async () => {
  // Both, not either: the error explains this session, the re-arm is what makes
  // the next one fix it.
  const { port, calls } = spyPort(() => Promise.reject('a bare string, not an Error'));
  await dischargeOwedKit(port);
  if (!calls.includes('error:a bare string, not an Error')) {
    throw new Error(`a non-Error rejection must still surface; got ${calls.join(',')}`);
  }
  if (calls[calls.length - 1] !== 'rearm') {
    throw new Error('the re-arm must be the last thing that happens on the failure arm');
  }
});
