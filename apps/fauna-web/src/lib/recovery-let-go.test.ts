// Deno tests for the Recovery-kit let-go's gates. Run via:
//
//     just web-unit-test
//
// Web's render-only-while-dead test for `docs/goal/ui/settings.md` § Recovery
// kit, the fifth act — tui's
// `the_let_go_renders_only_while_a_generation_is_dead_and_arms_on_the_word`
// is the reference.

import { letGoAnswer, letGoArmed, letGoRenders, type DeadGenerations } from './recovery-let-go.ts';

const WORD = 'LET GO';
const copy = {
  let_go_done: (v: { retired: string }) => `${v.retired} unreadable items were let go.`,
  let_go_kept: 'Some of this data became readable again and was kept.',
};
const nothingDead: DeadGenerations = { generation_ids: [], unreadable_status: null };
const oneDead: DeadGenerations = {
  generation_ids: ['07'.repeat(32)],
  unreadable_status: { key: 'settings.recovery_kit.unreadable_status_undated', args: { rows: '11' } },
};

Deno.test('the trio renders only while the shared line is present', () => {
  if (letGoRenders(null)) throw new Error('an unread dead list must not render the trio');
  if (letGoRenders(nothingDead)) throw new Error('nothing dead — the trio stays off the page');
  if (!letGoRenders(oneDead)) throw new Error('a dead generation renders the trio');
});

Deno.test('the button arms on the exact confirm word alone', () => {
  if (letGoArmed('', WORD, false)) throw new Error('unarmed until the word is typed');
  if (letGoArmed('let go', WORD, false)) {
    throw new Error('the confirm word is a fixed literal, not a case-insensitive match');
  }
  if (letGoArmed(' LET GO', WORD, false)) throw new Error('no trimming: the word is exact');
  if (!letGoArmed(WORD, WORD, false)) throw new Error('the confirm word arms it');
  if (letGoArmed(WORD, WORD, true)) throw new Error('busy holds it');
  if (letGoArmed(WORD, null, false)) throw new Error('no word read from the door — never armed');
});

Deno.test('the answer names the retired count, and a kept generation', () => {
  const done = letGoAnswer({ retired: 11, kept: false, dead: nothingDead }, copy);
  if (done !== '11 unreadable items were let go.') throw new Error(`got ${JSON.stringify(done)}`);
  const kept = letGoAnswer({ retired: 3, kept: true, dead: oneDead }, copy);
  if (kept !== '3 unreadable items were let go. Some of this data became readable again and was kept.') {
    throw new Error(`a kept generation is named, never skipped; got ${JSON.stringify(kept)}`);
  }
});
