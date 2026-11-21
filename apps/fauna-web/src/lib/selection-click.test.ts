// Text the person reads can be copied (`docs/goal/ui/README.md` § Text the
// person reads can be copied), on web:
//  1. `endsTextSelection` — a click that ends a drag-selection inside a row is
//     not a press; a plain click (collapsed selection) and a selection
//     elsewhere still are (rule 3, *selecting is not acting*).
//  2. No read-only text is made unselectable: no `user-select: none` (any
//     vendor prefix) and no Tailwind `select-none` anywhere in the SPA (rule 1).
//  3. Every clickable text row or card asks `endsTextSelection` in its click
//     handler before acting.
//
//     deno test --allow-read apps/fauna-web/src/lib/selection-click.test.ts

import { assert, assertEquals } from 'jsr:@std/assert';
import { endsTextSelection, snapshotSelection, type SelectionLike } from './selection-click.ts';

const inside = { id: 'inside' };
const outside = { id: 'outside' };
const row = { contains: (n: unknown) => n === inside };

const sel = (isCollapsed: boolean, anchorNode: unknown, focusNode: unknown): SelectionLike => ({
  isCollapsed,
  anchorNode,
  focusNode,
});

Deno.test('a click ending a selection made inside the row is not a press', () => {
  assert(endsTextSelection(row, sel(false, inside, inside)));
  assert(endsTextSelection(row, sel(false, outside, inside)), 'a drag that ends inside the row');
  assert(endsTextSelection(row, sel(false, inside, outside)), 'a drag that starts inside the row');
});

Deno.test('a plain click, or a selection elsewhere, still acts', () => {
  assertEquals(endsTextSelection(row, sel(true, inside, inside)), false);
  assertEquals(endsTextSelection(row, sel(false, outside, outside)), false);
  assertEquals(endsTextSelection(row, null), false);
  assertEquals(endsTextSelection(null, sel(false, inside, inside)), false);
});

Deno.test('a click whose press found the same selection standing is a plain click', () => {
  // The browser keeps a selection a press lands inside until the release, so
  // the click after a drag-select still sees it: the press found it, so it acts.
  const standing = sel(false, inside, inside);
  assertEquals(endsTextSelection(row, standing, snapshotSelection(standing)), false);
  // A drag that made a new selection: its press found none, or another one.
  assert(endsTextSelection(row, standing, null));
  assert(endsTextSelection(row, standing, snapshotSelection(sel(false, outside, outside))));
  assertEquals(snapshotSelection(sel(true, inside, inside)), null);
});

const SRC = new URL('../', import.meta.url);

async function* svelteAndStyleFiles(dir: URL): AsyncGenerator<URL> {
  for await (const e of Deno.readDir(dir)) {
    const u = new URL(e.name + (e.isDirectory ? '/' : ''), dir);
    if (e.isDirectory) yield* svelteAndStyleFiles(u);
    else if (/\.(svelte|css|ts)$/.test(e.name) && !e.name.endsWith('.test.ts')) yield u;
  }
}

Deno.test('no read-only text is made unselectable', async () => {
  const offenders: string[] = [];
  for await (const f of svelteAndStyleFiles(SRC)) {
    const code = await Deno.readTextFile(f);
    if (/user-select\s*:\s*none|userSelect\s*[:=]\s*['"]none|\bselect-none\b/.test(code)) {
      offenders.push(f.pathname);
    }
  }
  assertEquals(offenders, [], 'read-only text must stay selectable (README.md § Text the person reads can be copied)');
});

// [file under src/, the row's data-testid constant] — each row's click handler
// sits within a few lines after its test id and must ask `endsTextSelection`.
const ROWS: [string, string][] = [
  ['lib/components/PostCard.svelte', 'handleCardActivate'],
  ['routes/notifications/+page.svelte', 'IDS.NOTIFICATION_ITEM'],
  ['routes/search/+page.svelte', 'IDS.SEARCH_RESULT_ITEM'],
  ['routes/events/+page.svelte', 'IDS.EVENT_CARD'],
  ['routes/media/+page.svelte', 'IDS.MEDIA_ITEM'],
  ['routes/backups/+page.svelte', 'IDS.SNAPSHOT_ITEM'],
  ['lib/components/FoldersSection.svelte', 'IDS.FOLDER_ROW'],
  ['lib/components/NestRetirePage.svelte', 'IDS.RETIRE_SERVER_ITEM'],
];

Deno.test('every clickable text row asks endsTextSelection before acting', async () => {
  for (const [rel, anchor] of ROWS) {
    const code = await Deno.readTextFile(new URL(rel, SRC));
    const at = code.indexOf(anchor === 'handleCardActivate' ? 'function handleCardActivate' : anchor);
    assert(at >= 0, `${rel}: ${anchor} not found — did it move? update this contract`);
    const window = code.slice(at, at + 900);
    const click = window.indexOf('onclick');
    const fnStart = anchor === 'handleCardActivate' ? 0 : click;
    assert(fnStart >= 0, `${rel}: no onclick after ${anchor}`);
    assert(
      /endsTextSelection\(/.test(window.slice(fnStart, fnStart + 300)),
      `${rel}: the ${anchor} click must return early on endsTextSelection(e.currentTarget)`,
    );
  }
});
