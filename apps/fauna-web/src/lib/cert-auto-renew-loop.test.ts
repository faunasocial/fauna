// Deno test for `runCertAutoRenewLoop` — web's certificate auto-renew tick
// (`tls-certificates.md` § C.3, *Any synced admin device renews*): what the app
// owns around the shared `auto_renew_scan` / `auto_renew_issue`. Run via:
//
//     deno test apps/fauna-web/src/lib/cert-auto-renew-loop.test.ts

import { runCertAutoRenewLoop, type CertAutoRenewDeps } from './cert-auto-renew-loop.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

/** A loop driven for `passes` waits, recording every call the loop made. */
async function drive(
  passes: number,
  scans: string[][],
  resolve: () => Promise<number[]> = () => Promise.resolve([7, 7]),
) {
  const calls: string[] = [];
  let waits = 0;
  let stopped = false;
  const deps: CertAutoRenewDeps = {
    pollSecs: () => 21600,
    sleep: (ms) => {
      calls.push(`sleep ${ms}`);
      waits += 1;
      if (waits > passes) stopped = true;
      return Promise.resolve();
    },
    stopped: () => stopped,
    scan: () => {
      calls.push('scan');
      return Promise.resolve(scans.shift() ?? []);
    },
    resolveTargetNestId: () => {
      calls.push('resolve');
      return resolve();
    },
    issue: (domains, nestId) => {
      calls.push(`issue ${domains.join(',')} → ${nestId.join(',')}`);
      return Promise.resolve({ issued: domains, failed: [] });
    },
    warn: (line) => calls.push(`warn ${line}`),
    shipped: () => calls.push('shipped'),
  };
  await runCertAutoRenewLoop(deps);
  return calls;
}

Deno.test('the first pass waits a full period, then scans', async () => {
  const calls = await drive(1, [[]]);
  eq(calls.slice(0, 2), ['sleep 21600000', 'scan'], 'wait first, on the shared cadence');
});

Deno.test('a scan with nothing due resolves no nest id and issues nothing', async () => {
  const calls = await drive(2, [[], []]);
  eq(calls.filter((c) => c !== 'scan' && !c.startsWith('sleep')), [], 'no resolve, no issue, nothing shipped');
});

Deno.test('a due domain is issued to the bound nest, then the snapshot is shipped', async () => {
  const calls = await drive(1, [['a.test', 'b.test']]);
  eq(
    calls.slice(1),
    ['scan', 'resolve', 'issue a.test,b.test → 7,7', 'shipped', 'sleep 21600000'],
    'scan → resolve → issue → ship, then the next wait',
  );
});

Deno.test('a refused nest id skips the pass and the loop keeps going', async () => {
  const calls = await drive(2, [['a.test'], ['a.test']], () => Promise.reject(new Error('claim mismatch')));
  eq(calls.filter((c) => c.startsWith('issue')), [], 'nothing issued without the bound id');
  eq(calls.filter((c) => c === 'scan').length, 2, 'the next pass still runs');
  if (!calls.some((c) => c.startsWith('warn') && c.includes('claim mismatch'))) {
    throw new Error(`the refusal is logged: ${JSON.stringify(calls)}`);
  }
});

Deno.test('a failed scan never stops the loop', async () => {
  const calls: string[] = [];
  let waits = 0;
  await runCertAutoRenewLoop({
    pollSecs: () => 1,
    sleep: () => {
      waits += 1;
      return Promise.resolve();
    },
    stopped: () => waits > 2,
    scan: () => Promise.reject(new Error('socket closed')),
    resolveTargetNestId: () => Promise.resolve([1]),
    issue: () => Promise.resolve({ issued: [], failed: [] }),
    warn: (line) => calls.push(line),
    shipped: () => calls.push('shipped'),
  });
  eq(calls.length, 2, 'each failed pass is logged once and the loop goes on');
});
