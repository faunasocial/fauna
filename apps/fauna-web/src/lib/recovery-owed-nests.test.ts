// The owed-nests list's local fold — the dismiss drops by key, never by index.

import { owedKey, withoutOwed, type OwedNests } from './recovery-owed-nests.ts';

const line = { key: 'settings.recovery_kit.owed_nest_not_tried' } as never;
const entry = (keeper: string | null, nest: string) => ({
  keeper,
  old_actor_id: 'aa',
  nest_id: nest,
  address_copy: line,
  outcome_copy: line,
  outcome_kind: 'not-tried',
});
const view: OwedNests = { status: line, entries: [entry(null, '01'), entry('ff', '01')] };

Deno.test('the same owed nest at two keepers is two keys', () => {
  if (owedKey(view.entries[0]) === owedKey(view.entries[1])) {
    throw new Error('the keeper is part of the key');
  }
});

Deno.test('a settled dismiss drops only that entry', () => {
  const after = withoutOwed(view, owedKey(view.entries[1]));
  if (after?.entries.length !== 1 || after.entries[0].keeper !== null) {
    throw new Error('the bound entry must survive the linked one being dropped');
  }
});

Deno.test('the last entry dropped takes the section off the page', () => {
  const one: OwedNests = { status: line, entries: [entry(null, '01')] };
  if (withoutOwed(one, owedKey(one.entries[0])) !== null) throw new Error('nothing owed → null');
  if (withoutOwed(null, 'x') !== null) throw new Error('an unread list stays unread');
});
