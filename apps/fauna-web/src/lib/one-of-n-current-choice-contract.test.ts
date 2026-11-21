// One-of-N controls publish the current choice (`docs/goal/architecture/
// e2e-systematic-ui-walks.md` § Implementation status today, layer (c)): a choice
// re-chosen while already current passes the walk only when its registry attrs
// carry `state` on/active/selected/current or `selected` true. A source contract
// (testing.md convention 17) over the five routes: each control carries its marker
// bound to the same expression as its painted `class:active`; `conversation-sort`
// is a cycle, so it names the order in force instead of a state.
//
// Run via `just web-unit-test`, or:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/one-of-n-current-choice-contract.test.ts

import { assert } from 'jsr:@std/assert';
import { stripComments } from './source-contract.ts';

function route(path: string): string {
  return stripComments(Deno.readTextFileSync(new URL(`../routes/${path}`, import.meta.url)));
}

Deno.test('feed source entries publish data-state from their active expression', () => {
  const src = route('feed/+page.svelte');
  assert(/class:active=\{trendingSelected\}\s+data-state=\{trendingSelected \? 'on' : 'off'\}/.test(src));
  assert(
    /class:active=\{selectedFeed === null && !trendingSelected\}\s+data-state=\{selectedFeed === null && !trendingSelected \? 'on' : 'off'\}/
      .test(src),
  );
});

Deno.test('profile tabs are tabs publishing aria-selected', () => {
  const src = route('profile/[[actorId]]/+page.svelte');
  assert(/<nav class="tabs" role="tablist">/.test(src));
  for (const tab of ['posts', 'tiers']) {
    assert(
      new RegExp(`role="tab"\\s+aria-selected=\\{activeTab === '${tab}'\\}`).test(src),
      `profile ${tab} tab lacks aria-selected`,
    );
  }
});

Deno.test('calendar view buttons publish data-state from viewMode', () => {
  const src = route('events/+page.svelte');
  for (const mode of ['agenda', 'month', 'week', 'day']) {
    assert(
      new RegExp(`class:active=\\{viewMode === '${mode}'\\} data-state=\\{viewMode === '${mode}' \\? 'on' : 'off'\\}`)
        .test(src),
      `calendar ${mode} view lacks data-state`,
    );
  }
});

Deno.test('settings rail row publishes data-state from its active expression', () => {
  const src = route('settings/+layout.svelte');
  assert(/class:active=\{current === entry\.id\}\s+data-state=\{current === entry\.id \? 'on' : 'off'\}/.test(src));
});

Deno.test('conversation-sort names the order in force and is not a choice', () => {
  const src = route('conversations/+page.svelte');
  const btn = src.match(/<button data-testid=\{IDS\.CONVERSATION_SORT\}[^>]*>/)?.[0] ?? '';
  assert(btn.includes('sortOrderLabel(snap?.sort)'), 'tooltip is not the shared order label');
  assert(btn.includes('data-value={snap?.sort'), 'button publishes no value');
  assert(!btn.includes('data-state'), 'a cycle publishes a value, never a state');
});
