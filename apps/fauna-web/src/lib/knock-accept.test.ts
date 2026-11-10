// `acceptKnock`'s routing (account-offline-mutation.md § Implementation status
// today, the knock-accept composer). The composer's send-or-queue is faked
// here; it is pinned Rust-side (`fauna-client-contacts`'s
// `a_knock_accept_with_no_nest_is_queued_scoped_to_its_sender` and
// `a_knock_accept_with_the_nest_connected_is_sent_not_queued`).

import { acceptKnock, type AcceptKnockVerbs } from './knock-accept.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) throw new Error(`${msg}: expected ${String(expected)}, got ${String(actual)}`);
}

function recorder(queued: boolean) {
  const calls: string[] = [];
  const verbs: AcceptKnockVerbs = {
    acceptOrQueue: () => {
      calls.push('accept');
      return Promise.resolve(queued);
    },
    reread: () => {
      calls.push('reread');
      return Promise.resolve();
    },
  };
  return { calls, verbs };
}

Deno.test('a knock accept with no nest is queued and never re-read', async () => {
  const { calls, verbs } = recorder(true);
  eq(await acceptKnock(verbs), 'queued', 'queued');
  eq(calls.join(','), 'accept', 'queued: never re-read');
});

Deno.test('a knock accept with the nest connected is sent and re-read', async () => {
  const { calls, verbs } = recorder(false);
  eq(await acceptKnock(verbs), 'sent', 'sent');
  eq(calls.join(','), 'accept,reread', 'sent: re-read');
});
