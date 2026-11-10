// `createFilter`'s routing (account-offline-mutation.md § Implementation status
// today, the email filter-create composer). The composer's send-or-queue is
// faked here; it is pinned Rust-side (`fauna-client-email`'s
// `a_filter_create_with_no_nest_is_queued_scoped_to_its_name` and
// `a_filter_create_with_the_nest_connected_is_sent_not_queued`).

import { createFilter, type CreateFilterVerbs } from './email-filter-create.ts';
import type { EmailFilter } from './rpc.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) throw new Error(`${msg}: expected ${String(expected)}, got ${String(actual)}`);
}

function recorder(queued: boolean) {
  const calls: string[] = [];
  const listed = [{ id: 7 } as unknown as EmailFilter];
  const verbs: CreateFilterVerbs = {
    createOrQueue: () => {
      calls.push('create');
      return Promise.resolve(queued);
    },
    reread: () => {
      calls.push('reread');
      return Promise.resolve(listed);
    },
  };
  return { calls, verbs, listed };
}

Deno.test('a filter create with no nest is queued and lists nothing', async () => {
  const { calls, verbs } = recorder(true);
  const out = await createFilter(verbs);
  eq(out, null, 'queued: no list');
  eq(calls.join(','), 'create', 'queued: never re-read');
});

Deno.test('a filter create with the nest connected is sent and re-listed', async () => {
  const { calls, verbs, listed } = recorder(false);
  const out = await createFilter(verbs);
  eq(out, listed, 'sent: the re-read list');
  eq(calls.join(','), 'create,reread', 'sent: re-read');
});
