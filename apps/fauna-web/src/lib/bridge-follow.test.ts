// `changeFollow`'s routing (account-offline-mutation.md § Implementation status
// today, the bridges follow composer). The composer's send-or-queue is faked
// here; it is pinned Rust-side (`fauna-client-bridges`'
// `a_follow_added_with_no_nest_is_queued_scoped_to_the_account` and
// `follows_with_the_nest_connected_are_sent_not_queued`).

import { changeFollow } from './bridge-follow.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) throw new Error(`${msg}: expected ${String(expected)}, got ${String(actual)}`);
}

function recorder(queued: boolean) {
  const calls: string[] = [];
  const sendOrQueue = () => {
    calls.push('send');
    return Promise.resolve(queued);
  };
  const reread = () => {
    calls.push('reread');
    return Promise.resolve();
  };
  return { calls, sendOrQueue, reread };
}

Deno.test('a follow with no nest is queued and changes no list', async () => {
  const { calls, sendOrQueue, reread } = recorder(true);
  eq(await changeFollow(sendOrQueue, reread), true, 'queued');
  eq(calls.join(','), 'send', 'queued: never re-read');
});

Deno.test('a follow with the nest connected is sent and re-listed', async () => {
  const { calls, sendOrQueue, reread } = recorder(false);
  eq(await changeFollow(sendOrQueue, reread), false, 'sent');
  eq(calls.join(','), 'send,reread', 'sent: re-read');
});
