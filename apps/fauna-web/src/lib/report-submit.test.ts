// `submitReport`'s routing (account-offline-mutation.md § Implementation status
// today, the abuse-report composer). The composer's send-or-queue is faked
// here; it is pinned Rust-side (`fauna-client-moderation`'s
// `a_report_with_no_nest_is_queued_scoped_to_its_subject` and
// `a_report_with_the_nest_connected_is_sent_not_queued`).

import { submitReport, type ReportSubmitVerbs } from './report-submit.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) throw new Error(`${msg}: expected ${String(expected)}, got ${String(actual)}`);
}

function recorder(queued: boolean) {
  const calls: string[] = [];
  const verbs: ReportSubmitVerbs<{ report_id: string }> = {
    submitOrQueue: () => {
      calls.push('submit');
      return Promise.resolve(queued ? null : { report_id: 'r1' });
    },
    block: () => {
      calls.push('block');
      return Promise.resolve();
    },
    hide: () => {
      calls.push('hide');
      return Promise.resolve(['cid1']);
    },
    blockNotSent: () => 'BLOCK NOT SENT',
  };
  return { calls, verbs };
}

Deno.test('a report with no nest is queued, hides, and does not try the block', async () => {
  const { calls, verbs } = recorder(true);
  const out = await submitReport(verbs);
  eq(out.reply, null, 'queued: no reply');
  eq(out.blocked, false, 'queued: no block');
  eq(out.hidden?.join(','), 'cid1', 'queued: the hide still lands');
  eq(out.followup, 'BLOCK NOT SENT', 'queued: the error line says the block was not sent');
  eq(calls.join(','), 'submit,hide', 'queued: never blocks');
});

Deno.test('a report with the nest connected is sent, blocks and hides', async () => {
  const { calls, verbs } = recorder(false);
  const out = await submitReport(verbs);
  eq(out.reply?.report_id, 'r1', 'sent: the reply');
  eq(out.blocked, true, 'sent: blocked');
  eq(out.followup, null, 'sent: no follow-up');
  eq(calls.join(','), 'submit,block,hide', 'sent: full sequence');
});
