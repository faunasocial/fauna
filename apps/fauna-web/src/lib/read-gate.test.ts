// Deno tests for the read-state half of an affordance's answer
// (`./read-gate.ts`; `docs/goal/ui/README.md` § Offer only what is possible,
// rules 2, 5, 6). Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/read-gate.test.ts
//
// The answers are shared Rust and pinned there (`fauna_core::possibility`,
// `fauna_client_features::gate_possibility`, `fauna_client_notifications`);
// the stand-ins below answer as those do, so these tests exercise web's own
// part — which read word the page holds, which answer it asks, and that the
// paint and the handler's refusal come from that one answer.

import { formGate, formRefusal } from './form-gate.ts';
import { resolveLocalizedNested } from './i18n/localized.ts';
import { t } from './i18n/strings.ts';
import type { Possibility } from './possibility.ts';
import {
  andPossibility,
  blockAnswer,
  featureAnswer,
  followAnswer,
  readFailed,
  type ReadWord,
} from './read-gate.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

/** `Possibility::from_read`'s answers. */
function readPossibility(read: ReadWord): Possibility {
  switch (read) {
    case 'pending':
      return { state: 'blocked', reason: { key: 'common.still_loading' } };
    case 'failed':
      return { state: 'blocked', reason: { key: 'common.load_failed' } };
    case 'resolved':
      return { state: 'possible' };
  }
}

interface Row {
  feature: string;
  shown: boolean;
  restriction: { key: string; args?: Record<string, string> } | null;
}

/** `gate_possibility`'s shapes: no rows → still loading; a hidden or missing
 *  row → absent; a restricted row → its restriction; else possible. */
function featuresGatePossibility(rows: Row[] | null, feature: string): Possibility {
  if (rows === null) return readPossibility('pending');
  const row = rows.find((r) => r.feature === feature);
  if (!row || !row.shown) return { state: 'absent' };
  return row.restriction ? { state: 'blocked', reason: row.restriction } : { state: 'possible' };
}

const asks = { readPossibility, featuresGatePossibility };
const STILL_LOADING = { shown: true, disabled: true, reason: t.common.still_loading };
const LOAD_FAILED = { shown: true, disabled: true, reason: t.common.load_failed };
const LIVE = { shown: true, disabled: false, reason: '' };

Deno.test('a failed read stays failed until it answers, and an answered read stays answered', () => {
  eq(readFailed('pending'), 'failed', 'first read failed');
  eq(readFailed('failed'), 'failed', 'still failed');
  eq(readFailed('resolved'), 'resolved', 'a later failure keeps the rows shown actionable');
});

Deno.test('and: absent wins, then the first blocked reason, else the second answer', () => {
  const a: Possibility = { state: 'blocked', reason: { key: 'a' } };
  const b: Possibility = { state: 'blocked', reason: { key: 'b' } };
  eq(andPossibility({ state: 'possible' }, { state: 'possible' }), { state: 'possible' }, 'both possible');
  eq(andPossibility({ state: 'possible' }, b), b, 'second blocks');
  eq(andPossibility(a, b), a, 'the page gate speaks first');
  eq(andPossibility(a, { state: 'absent' }), { state: 'absent' }, 'absent wins');
  eq(andPossibility({ state: 'absent' }, { state: 'possible' }), { state: 'absent' }, 'absent wins first');
});

Deno.test('follow waits on the status read: loading, then failed, then live — or live once pressed', () => {
  eq(formGate(followAnswer(readPossibility, false, 'pending')), STILL_LOADING, 'pending');
  eq(formGate(followAnswer(readPossibility, false, 'failed')), LOAD_FAILED, 'failed read');
  eq(formGate(followAnswer(readPossibility, false, 'resolved')), LIVE, 'answered');
  eq(formGate(followAnswer(readPossibility, true, 'failed')), LIVE, 'the viewer pressed it this open');
  // The handler refuses with the same reason it painted, sending nothing.
  eq(formRefusal(followAnswer(readPossibility, false, 'pending')), t.common.still_loading, 'pending refusal');
  eq(formRefusal(followAnswer(readPossibility, false, 'failed')), t.common.load_failed, 'failed refusal');
  eq(formRefusal(followAnswer(readPossibility, false, 'resolved')), null, 'answered: goes on');
});

Deno.test('block waits on the edge read and refuses before it with the painted reason', () => {
  eq(formGate(blockAnswer(readPossibility, 'pending')), STILL_LOADING, 'edge unknown');
  eq(formRefusal(blockAnswer(readPossibility, 'pending')), t.common.still_loading, 'refusal');
  eq(formRefusal(blockAnswer(readPossibility, 'failed')), t.common.load_failed, 'failed read');
  eq(formGate(blockAnswer(readPossibility, 'resolved')), LIVE, 'edge known');
});

Deno.test('a gated feature is loading before its read, failed after a failed one, absent without its row', () => {
  const paint = (rows: Row[] | null, read: ReadWord) =>
    formGate(featureAnswer(asks, rows, read, 'payments'), resolveLocalizedNested);
  eq(paint(null, 'pending'), STILL_LOADING, 'not read yet');
  eq(paint(null, 'failed'), LOAD_FAILED, 'read failed, no rows in hand');
  eq(paint([], 'resolved').shown, false, 'no payments row: the whole block is left out');
  eq(paint([{ feature: 'payments', shown: false, restriction: null }], 'resolved').shown, false, 'hidden row');
  eq(paint([{ feature: 'payments', shown: true, restriction: null }], 'resolved'), LIVE, 'available');
  // Rows already in hand outlive a later failed refresh.
  eq(paint([{ feature: 'payments', shown: true, restriction: null }], 'failed'), LIVE, 'later failure');
});

Deno.test("a restricted feature's reason resolves its nested window word, on paint and on refusal alike", () => {
  const restriction = { key: 'features.exhausted_admin', args: { window: 'features.window_day' } };
  const answer = featureAnswer(asks, [{ feature: 'zaps', shown: true, restriction }], 'resolved', 'zaps');
  const painted = formGate(answer, resolveLocalizedNested);
  eq(painted.disabled, true, 'blocked');
  eq(painted.reason, resolveLocalizedNested(restriction), 'nested resolution');
  eq(painted.reason.includes('features.window_day'), false, 'no raw nested key on screen');
  eq(formRefusal(answer, resolveLocalizedNested), painted.reason, 'refusal = painted reason');
});

Deno.test('zap designate: the plane gate speaks before the draft, and is absent without the plane', () => {
  const draftEmpty: Possibility = { state: 'blocked', reason: { key: 'nostr.zap_signers.pubkey_placeholder' } };
  const plane = (rows: Row[] | null, read: ReadWord) => featureAnswer(asks, rows, read, 'zaps');
  eq(formGate(andPossibility(plane(null, 'pending'), draftEmpty), resolveLocalizedNested), STILL_LOADING, 'before the read');
  eq(
    formGate(andPossibility(plane([], 'resolved'), { state: 'possible' }), resolveLocalizedNested).shown,
    false,
    'no zaps row: not shown',
  );
  eq(
    formGate(andPossibility(plane([{ feature: 'zaps', shown: true, restriction: null }], 'resolved'), { state: 'possible' })),
    LIVE,
    'plane open, draft good',
  );
});

Deno.test('notifications: the page-wide actions refuse before the first read with its reason', () => {
  // `mark_all_read_possibility` / `clear_all_possibility`'s shapes.
  const markRead = (read: ReadWord) => readPossibility(read);
  const clearAll = (read: ReadWord, rows: number): Possibility =>
    read !== 'resolved'
      ? readPossibility(read)
      : rows === 0
      ? { state: 'blocked', reason: { key: 'notifications.nothing_to_clear' } }
      : { state: 'possible' };
  eq(formGate(markRead('pending')), STILL_LOADING, 'mark-read pending');
  eq(formGate(clearAll('pending', 0)), STILL_LOADING, 'clear pending');
  eq(formRefusal(clearAll('failed', 0)), t.common.load_failed, 'clear failed refusal');
  eq(formRefusal(clearAll('resolved', 0)), t.notifications.nothing_to_clear, 'empty page refusal');
  eq(formRefusal(clearAll('resolved', 2)), null, 'rows: goes on');
});
