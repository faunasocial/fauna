// Deno test for web's half of the failed-export record
// (`docs/goal/behavior/mail-export.md` § A failed export is a record the wizard
// shows until the user moves on). Mirrors tui's
// `a_failed_export_offers_dismiss_and_none_of_the_three_controls`; the
// component is pinned to the helper by a source check, as
// `mail-export-confirm-estimate.test.ts` pins its own summary.
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`

import { assert, assertEquals } from 'jsr:@std/assert';
import { isFailedExport } from './mail-export-failed.ts';
import { t } from './i18n/strings.ts';

Deno.test('only an Errored session is a failed export', () => {
  assertEquals(isFailedExport('Errored'), true);
  for (const live of ['Running', 'Paused', 'Completed', 'Cancelled', null, undefined]) {
    assertEquals(isFailedExport(live), false, String(live));
  }
});

Deno.test('the failed Progress step paints Dismiss in place of the three controls', async () => {
  assertEquals(t.mail_export.dismiss_failed_button, 'Dismiss');
  const src = await Deno.readTextFile(new URL('./components/MailExportSection.svelte', import.meta.url));
  const progress = src.slice(src.indexOf('Step 4 — Progress'), src.indexOf('Step 5 — Done'));
  assert(progress.includes('isFailedExport(snap.session_state)'));
  const failedBranch = progress.slice(progress.indexOf('{#if isFailedExport'), progress.indexOf('{:else}'));
  assert(failedBranch.includes('IDS.MAIL_EXPORT_DISCARD_BUTTON'));
  assert(failedBranch.includes('t.mail_export.dismiss_failed_button'));
  for (const id of ['PAUSE', 'RESUME', 'CANCEL']) {
    assert(!failedBranch.includes(`MAIL_EXPORT_${id}_BUTTON`), id);
  }
  const live = progress.slice(progress.indexOf('{:else}'));
  for (const id of ['PAUSE', 'RESUME', 'CANCEL']) {
    assert(live.includes(`MAIL_EXPORT_${id}_BUTTON`), id);
  }
});
