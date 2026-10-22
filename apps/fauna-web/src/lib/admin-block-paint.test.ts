// Deno test for `adminBlockPaint` — which shape a DM bubble paints an admin block's notice in
// (`moderation.md` § Admin block of a reported item, decision 4). Run via:
//
//     deno test apps/fauna-web/src/lib/admin-block-paint.test.ts
//
// A withheld record (empty document) paints the notice in place of the bubble; the author's own
// blocked message (non-empty document) keeps its body with the notice beside it. The rule is the
// Rust `MessageSnapshot::admin_block_in_place`; this pins the web face of it.

import { adminBlockPaint, withheldCarriesBlockNotice } from './admin-block-paint.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) throw new Error(`${msg}\n  expected: ${expected}\n  actual:   ${actual}`);
}

const empty = { blocks: [] };
const withBody = { blocks: [{ Paragraph: { inlines: [] } }] };

Deno.test('a withheld record (empty document) paints the notice in place', () => {
  eq(adminBlockPaint('harassment', empty), 'in-place', 'empty document');
  eq(adminBlockPaint('harassment', undefined), 'in-place', 'no document');
});

Deno.test("the author's own blocked message keeps its body with the notice beside it", () => {
  eq(adminBlockPaint('harassment', withBody), 'beside', 'non-empty document');
});

Deno.test('an unblocked message paints no notice', () => {
  eq(adminBlockPaint(null, withBody), 'none', 'null reason, body');
  eq(adminBlockPaint(undefined, empty), 'none', 'absent reason, empty');
});

Deno.test('a Blocked withheld badge carries the notice; a TakenDown one and none do not', () => {
  eq(withheldCarriesBlockNotice({ ground: 'Blocked' }), true, 'blocked');
  eq(withheldCarriesBlockNotice({ ground: 'TakenDown' }), false, 'taken down');
  eq(withheldCarriesBlockNotice(null), false, 'no badge');
  eq(withheldCarriesBlockNotice(undefined), false, 'absent');
});
