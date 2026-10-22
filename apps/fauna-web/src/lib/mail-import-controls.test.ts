// Deno test for web's half of the mail-import controls lifted from tui:
// the failed-import Dismiss, the restart offer, the import-duplicates toggle
// and the forget button. Web's wasm never wires the source memory and web's
// import never starts a run, so the witness here is a render test over made-up
// snapshots (a source of truth the e2e journeys cannot reach on web).
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`

import { assert, assertEquals } from 'jsr:@std/assert';
import { isFailedImport, nextSourceDrafts, restartOffered } from './mail-import-controls.ts';
import { t } from './i18n/strings.ts';

Deno.test('only an Errored session is a failed import', () => {
  assertEquals(isFailedImport('Errored'), true);
  for (const live of ['Running', 'Paused', 'Completed', 'Cancelled', null, undefined]) {
    assertEquals(isFailedImport(live), false, String(live));
  }
});

Deno.test('the restart is offered only while a rebuilt mailbox is pending', () => {
  assertEquals(restartOffered([]), false);
  assertEquals(restartOffered(undefined), false);
  assertEquals(restartOffered(['INBOX']), true);
});

Deno.test('the remembered source fills the drafts once and Forget empties them', () => {
  const remembered = { source_remembered: true, host: 'imap.old.example', port: 993, username: 'me@old.example' };
  const first = nextSourceDrafts(remembered, true, false, false);
  assertEquals(first?.host, 'imap.old.example');
  assertEquals(first?.port, '993');
  assertEquals(first?.username, 'me@old.example');
  assertEquals(first?.seeded, true);
  // Never over what the user typed, never twice in one visit.
  assertEquals(nextSourceDrafts(remembered, false, false, false), null);
  assertEquals(nextSourceDrafts(remembered, true, true, true), null);
  // A landed Forget empties the drafts and the typed password.
  const forgotten = { ...remembered, source_remembered: false, host: '', port: 0, username: '' };
  const cleared = nextSourceDrafts(forgotten, false, true, true);
  assertEquals(cleared?.host, '');
  assertEquals(cleared?.username, '');
  assertEquals(cleared?.clearPassword, true);
});

Deno.test('the component paints the four controls from the snapshot', async () => {
  const src = await Deno.readTextFile(new URL('./components/MailImportSection.svelte', import.meta.url));
  const progress = src.slice(src.indexOf('Step 4 — Progress'), src.indexOf('Step 5 — Done'));
  assert(progress.includes('isFailedImport(snap.session_state)'));
  const failedBranch = progress.slice(progress.indexOf('{#if isFailedImport'), progress.indexOf('{:else}'));
  assert(failedBranch.includes('IDS.MAIL_IMPORT_DISMISS_BUTTON'));
  assert(failedBranch.includes('t.mail_import.dismiss_failed_button'));
  for (const id of ['PAUSE', 'RESUME', 'CANCEL', 'RESTART']) {
    assert(!failedBranch.includes(`MAIL_IMPORT_${id}_BUTTON`), id);
  }
  const live = progress.slice(progress.indexOf('{:else}'));
  for (const id of ['PAUSE', 'RESUME', 'CANCEL', 'RESTART']) {
    assert(live.includes(`MAIL_IMPORT_${id}_BUTTON`), id);
  }
  assert(live.includes('restartOffered(snap.rebuilt_mailboxes)'));
  assert(live.includes("dispatch('RestartRebuiltMailboxes')"));
  assertEquals(t.mail_import.dismiss_failed_button, 'Dismiss');
  assertEquals(t.mail_import.restart_button, 'Restart rebuilt mailbox');

  const source = src.slice(src.indexOf('Step 1 — Source'), src.indexOf('Step 2 — Scope'));
  assert(source.includes('{#if snap.source_remembered}'));
  assert(source.includes('IDS.MAIL_IMPORT_SOURCE_FORGET_BUTTON'));
  assert(source.includes("dispatch('ForgetSource')"));

  const scope = src.slice(src.indexOf('Step 2 — Scope'), src.indexOf('Step 3 — Confirm'));
  assert(scope.includes('IDS.MAIL_IMPORT_SCOPE_IMPORT_DUPLICATES_TOGGLE'));
  assert(scope.includes('t.mail_import.scope_import_duplicates_label'));
  assert(scope.includes("data-state={snap.import_duplicates ? 'on' : 'off'}"));
  assert(scope.includes('SetImportDuplicates'));
});
