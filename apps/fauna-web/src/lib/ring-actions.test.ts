// The Logs pages' copy and clear (`ring-actions.ts`) and a copy button's
// outcome (`copy-outcome.ts`; `docs/goal/ui/README.md` § Offer only what is
// possible, rules 2 and 7). `ringAnswer` stands in for the shared
// `fauna_log::format::ring_action_possibility` (pinned in Rust): no entry
// shown → blocked by the page's own empty line, else possible.
import { assertEquals } from 'jsr:@std/assert@1';
import { copyOutcome } from './copy-outcome.ts';
import { formGate } from './form-gate.ts';
import { t } from './i18n/strings.ts';
import type { Possibility } from './possibility.ts';
import { ADMIN_LOGS_EMPTY_KEY, DEVICE_LOGS_EMPTY_KEY, clearRing, copyRing } from './ring-actions.ts';

function ringAnswer(shown: number, key: string): Possibility {
  return shown === 0 ? { state: 'blocked', reason: { key } } : { state: 'possible' };
}

const ok = (text: string) => copyOutcome(text, () => Promise.resolve());
const refused = (text: string) =>
  copyOutcome(text, () => Promise.reject(new Error('denied')));

Deno.test('an empty ring paints copy and clear disabled with the empty line', () => {
  assertEquals(formGate(ringAnswer(0, DEVICE_LOGS_EMPTY_KEY)), {
    shown: true,
    disabled: true,
    reason: t.logs.empty,
  });
  assertEquals(formGate(ringAnswer(0, ADMIN_LOGS_EMPTY_KEY)).reason, t.admin.logs_page.empty);
  assertEquals(formGate(ringAnswer(3, DEVICE_LOGS_EMPTY_KEY)).disabled, false);
});

Deno.test('a copy on an empty ring is refused with the empty line, nothing copied', async () => {
  let wrote = false;
  const result = await copyRing(
    () => ringAnswer(0, DEVICE_LOGS_EMPTY_KEY),
    () => '',
    (text) => copyOutcome(text, () => {
      wrote = true;
      return Promise.resolve();
    }),
  );
  assertEquals(result, { copied: null, error: t.logs.empty });
  assertEquals(wrote, false);
});

Deno.test('a copy that reached the clipboard is confirmed with the copied text', async () => {
  const result = await copyRing(() => ringAnswer(2, DEVICE_LOGS_EMPTY_KEY), () => 'ERROR · boom', ok);
  assertEquals(result, { copied: 'ERROR · boom', error: '' });
});

Deno.test('a failed clipboard write puts copy_failed on error-message', async () => {
  const result = await copyRing(() => ringAnswer(2, ADMIN_LOGS_EMPTY_KEY), () => 'x', refused);
  assertEquals(result, { copied: null, error: t.common.copy_failed({ error: 'denied' }) });
});

Deno.test('a clear on an empty ring is refused; a shown ring clears', () => {
  let cleared = 0;
  assertEquals(clearRing(() => ringAnswer(0, DEVICE_LOGS_EMPTY_KEY), () => cleared++), t.logs.empty);
  assertEquals(cleared, 0);
  assertEquals(clearRing(() => ringAnswer(1, DEVICE_LOGS_EMPTY_KEY), () => cleared++), null);
  assertEquals(cleared, 1);
});

Deno.test('the ring answer is unreadable before wasm: disabled, refusal still_loading', async () => {
  const throws = (): Possibility => {
    throw new Error('wasm not initialized');
  };
  const result = await copyRing(throws, () => 'x', ok);
  assertEquals(result, { copied: null, error: t.common.still_loading });
});

// The invite code's copy (`admin-users-invite-code-copy-btn`) rides the same
// outcome: confirmed with the code, or `copy_failed` on
// `admin-users-action-error`.
Deno.test('an invite-code copy confirms the code or says copy_failed', async () => {
  assertEquals(await ok('ABCD-1234'), { copied: 'ABCD-1234', failed: null });
  assertEquals(await refused('ABCD-1234'), {
    copied: null,
    failed: t.common.copy_failed({ error: 'denied' }),
  });
  const sync = await copyOutcome('c', () => {
    throw new TypeError('no clipboard');
  });
  assertEquals(sync.failed, t.common.copy_failed({ error: 'no clipboard' }));
});
