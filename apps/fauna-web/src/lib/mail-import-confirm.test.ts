// Deno tests for the mail-import Confirm summary. Run via:
//
//     deno test apps/fauna-web/src/lib/mail-import-confirm.test.ts
//
// Web cannot reach Confirm end to end (no source transport —
// `mailbox-migration.md` § Where the IMAP client runs), so the sized and
// unsized renders are proven here rather than by an e2e walk.

import { type ConfirmTemplates, confirmSummaryText } from './mail-import-confirm.ts';

const templates: ConfirmTemplates = {
  plain: (v) => `${v.source} · ${v.mailboxes} mailbox(es) · ${v.messages} messages`,
  sized: (v) =>
    `${v.source} · ${v.mailboxes} mailbox(es) · ${v.messages} messages · about ${v.size}`,
};
const byteSize = (b: number) => `${b} B`;

Deno.test('a sized estimate names the size through the sized template', () => {
  const got = confirmSummaryText(
    'Gmail',
    { mailboxes: 2, messages: 3, bytes: 1500 },
    templates,
    byteSize,
  );
  if (got !== 'Gmail · 2 mailbox(es) · 3 messages · about 1500 B') {
    throw new Error(`unexpected summary: ${got}`);
  }
});

Deno.test('an unsized estimate keeps the count and names no size', () => {
  const got = confirmSummaryText(
    'Gmail',
    { mailboxes: 2, messages: 3, bytes: null },
    templates,
    byteSize,
  );
  if (got !== 'Gmail · 2 mailbox(es) · 3 messages') {
    throw new Error(`unexpected summary: ${got}`);
  }
});

Deno.test('a zero-byte estimate is still sized, not unsized', () => {
  const got = confirmSummaryText(
    'Gmail',
    { mailboxes: 1, messages: 0, bytes: 0 },
    templates,
    byteSize,
  );
  if (!got.endsWith('about 0 B')) throw new Error(`unexpected summary: ${got}`);
});
