// Deno test for `archiveRowParts` — web's twin of tui's tier_1 pin
// `contacts::tests::the_archive_link_prompt_renders_only_on_its_asked_row_and_unlink_only_when_linked`
// (`archive-import.md` § Linking and merging, ruling 8). Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/archive-link-rows.test.ts

import { archiveRowParts, type ArchiveLinkState, type ArchiveLinkView } from './archive-link-rows.ts';

function assertEq<T>(actual: T, expected: T) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`expected ${e}, got ${a}`);
}

const view = (contact: string, state: ArchiveLinkState, unmatched = 0): ArchiveLinkView => ({
  contact,
  state,
  platform: 'facebook',
  archived_name: 'Friend Two',
  claimed: 0,
  unmatched,
});

const A = 'aa'.repeat(32);
const B = 'bb'.repeat(32);
const C = 'cc'.repeat(32);

Deno.test('the prompt renders only on its asked row and unlink only when linked', () => {
  const views = [view(A, 'Offered'), view(B, 'Asked'), view(C, 'Linked', 2)];
  assertEq(archiveRowParts(views, A), { asked: [], linked: [] });
  assertEq(archiveRowParts(views, B), { asked: [view(B, 'Asked')], linked: [] });
  assertEq(archiveRowParts(views, C), { asked: [], linked: [view(C, 'Linked', 2)] });
});

Deno.test('a declined or unlinked pair paints nothing', () => {
  const views = [view(B, 'Declined'), view(C, 'Unlinked')];
  assertEq(archiveRowParts(views, B), { asked: [], linked: [] });
  assertEq(archiveRowParts(views, C), { asked: [], linked: [] });
});

Deno.test('the roster hex matches the view case-insensitively', () => {
  const views = [view(B, 'Asked')];
  assertEq(archiveRowParts(views, B.toUpperCase()).asked.length, 1);
});
