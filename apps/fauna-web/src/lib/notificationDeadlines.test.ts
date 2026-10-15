// Run via: deno test apps/fauna-web/src/lib/notificationDeadlines.test.ts

import { assertEquals } from 'jsr:@std/assert';
import { epochSecsLocal, withLocalDeadlines } from './notificationDeadlines.ts';
import type { NotificationText } from './wasm';

const row = (args: Record<string, string>): NotificationText =>
  ({ kind: 'localized', key: 'row_security_pending_action_queued', args }) as NotificationText;

Deno.test('numeric deadline args become local time', () => {
  const out = withLocalDeadlines(row({ execute_after: '1758412800', lands_at: '1700000000' }));
  if (out.kind !== 'localized') throw new Error('expected localized');
  assertEquals(out.args!.execute_after, epochSecsLocal(1758412800));
  assertEquals(out.args!.lands_at, epochSecsLocal(1700000000));
  assertEquals(out.args!.execute_after === '1758412800', false);
});

Deno.test('non-numeric deadline passes through', () => {
  const out = withLocalDeadlines(row({ lands_at: 'soon' }));
  if (out.kind !== 'localized') throw new Error('expected localized');
  assertEquals(out.args!.lands_at, 'soon');
});

Deno.test('other args untouched, verbatim rows unchanged', () => {
  const out = withLocalDeadlines(row({ device: '1758412800', execute_after: '5' }));
  if (out.kind !== 'localized') throw new Error('expected localized');
  assertEquals(out.args!.device, '1758412800');
  const v: NotificationText = { kind: 'verbatim', text: 'x' };
  assertEquals(withLocalDeadlines(v), v);
});
