// Deno test for web's half of Review skipped on the mail import's Done screen
// (`docs/goal/behavior/mailbox-migration.md` § Wizard steps step 6). Mirrors
// tui's `show_skipped` toggle. Web cannot reach Done end to end (no source
// transport — § Implementation status today), so the render is pinned to the
// component by a source check, as `mail-export-failed.test.ts` pins its own.
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`

import { assert } from 'jsr:@std/assert';

const src = await Deno.readTextFile(
  new URL('./components/MailImportSection.svelte', import.meta.url),
);
const done = src.slice(src.indexOf('Step 5 — Done'), src.indexOf('</section>'));

Deno.test('Review skipped is a toggle button, not a link to Conversations', () => {
  const button = done.slice(
    done.indexOf('IDS.MAIL_IMPORT_REVIEW_SKIPPED_BUTTON') - 40,
    done.indexOf('t.mail_import.review_skipped_button'),
  );
  assert(button.includes('<button'), 'Review skipped must be a button');
  assert(button.includes('showSkipped = !showSkipped'), 'it must toggle showSkipped');
  assert(!button.includes('href='), 'it must not navigate away');
});

Deno.test('the skip log paints on the Done screen only while showSkipped is set', () => {
  const reveal = done.slice(done.indexOf('{#if showSkipped}'));
  assert(reveal.includes('IDS.MAIL_IMPORT_ERROR_LOG'));
  assert(reveal.includes('snap.error_log'));
  assert(reveal.includes('t.mail_import.error_log_title'));
});

Deno.test('leaving Done clears showSkipped so a fresh visit hides the log', () => {
  assert(src.includes("if (snap?.step !== 'Done') showSkipped = false;"));
});
