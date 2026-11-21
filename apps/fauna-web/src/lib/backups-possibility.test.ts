// The Backups page's controls paint from the shared backups answers
// (`docs/goal/ui/backups.md` § Errors & edge cases → *What the page offers*;
// `docs/goal/ui/README.md` § Offer only what is possible, rules 2, 4, 6, 7),
// and every gesture asks the same answer again before it runs. The wasm faces
// are stood in for by fakes answering in their serde JSON, with the shared
// rules the faces call reproduced only as far as each test needs.
import { assert, assertEquals } from 'jsr:@std/assert@1';
import {
  carveOutPaint,
  carveOutRefusal,
  deleteAnswer,
  deletePaint,
  deleteRefusal,
  destinationForm,
  destinationFormPaint,
  destinationFormRefusal,
  immediateDeletePaint,
  refuse,
  restoreInputsPaint,
  restorePaint,
  restoreRefusal,
  setActionPaint,
  setActionRefusal,
  type BackupsFaces,
  type BackupsMachineFaces,
} from './backups-possibility.ts';

const blocked = (key: string) => JSON.stringify({ state: 'blocked', reason: { key } });
const possible = JSON.stringify({ state: 'possible' });

/** The free faces, answering as `fauna_backups_machine::possibility` and
 *  `carve_out_affordance` do for the cases below. */
const faces: BackupsFaces = {
  destinationFormPossibilityJson: (form, url) =>
    (form === 'add_nest' || form === 'edit_nest') && url.trim() === ''
      ? blocked('backups.backup_destination_url_required')
      : possible,
  restoreInputsPossibilityJson: (id) =>
    id == null ? blocked('backups.restore_no_snapshots') : possible,
  restorePossibilityJson: (running, id, ticked, typed) =>
    id == null
      ? blocked('backups.restore_no_snapshots')
      : running
        ? blocked('backups.restore_progress_running')
        : !ticked
          ? blocked('backups.restore_kind_unticked')
          : typed !== id
            ? blocked('backups.restore_progress_idle')
            : possible,
  carveOutPossibilityJson: (word) =>
    ['connecting', 'disconnected', 'unreachable'].includes(word)
      ? blocked('common.needs_nest')
      : possible,
};

/** A machine whose rows are `states` — a pending row refuses the soft delete. */
function machine(states: string[], setAction = possible): BackupsMachineFaces {
  return {
    setActionPossibilityJson: () => setAction,
    deletePossibilityJson: (i) =>
      i >= states.length
        ? JSON.stringify({ state: 'absent' })
        : states[i] === 'pending'
          ? blocked('backups.snapshot_delete_already_pending')
          : possible,
    immediateDeletePossibilityJson: (i) =>
      i >= states.length ? JSON.stringify({ state: 'absent' }) : possible,
  };
}

// ── The destination dialog ──────────────────────────────────────────

Deno.test('the dialog names its form: an add is a nest add, an edit follows the row kind', () => {
  assertEquals(destinationForm(null), 'add_nest');
  assertEquals(destinationForm({ isClientDevice: false }), 'edit_nest');
  assertEquals(destinationForm({ isClientDevice: true }), 'edit_client_device');
});

Deno.test('a this-device rename saves with a blank URL', () => {
  const form = destinationForm({ isClientDevice: true });
  const paint = destinationFormPaint(faces, form, '', '');
  assert(paint.shown && !paint.disabled);
  assertEquals(destinationFormRefusal(faces, form, '', ''), null);
});

Deno.test('a nest form with a blank URL paints and refuses with the URL-required reason', () => {
  for (const form of ['add_nest', 'edit_nest'] as const) {
    const paint = destinationFormPaint(faces, form, '  ', '');
    assert(paint.shown && paint.disabled);
    assertEquals(paint.reason?.key, 'backups.backup_destination_url_required');
    assertEquals(
      destinationFormRefusal(faces, form, '  ', '')?.key,
      'backups.backup_destination_url_required',
    );
    assert(!destinationFormPaint(faces, form, 'https://b.example', '').disabled);
  }
});

Deno.test('the dialog confirm is dead with no reason before the backups chunk loads', () => {
  const paint = destinationFormPaint(null, 'add_nest', 'https://b.example', '');
  assert(paint.shown && paint.disabled);
  assertEquals(paint.reason, null);
});

// ── The snapshot half ───────────────────────────────────────────────

Deno.test('a pending row refuses the soft delete with its reason; the hard delete stays', () => {
  const m = machine(['active', 'pending']);
  assert(!deletePaint(m, 0).disabled);
  const pending = deletePaint(m, 1);
  assert(pending.shown && pending.disabled);
  assertEquals(pending.reason?.key, 'backups.snapshot_delete_already_pending');
  assertEquals(deleteRefusal(m, 1)?.key, 'backups.snapshot_delete_already_pending');
  assertEquals(deleteRefusal(m, 0), null);
  assert(!immediateDeletePaint(m, 1).disabled);
});

Deno.test('the set actions paint and refuse from the machine answer', () => {
  const m = machine([], blocked('backups.snapshot_select_folder_first'));
  const paint = setActionPaint(m);
  assert(paint.disabled);
  assertEquals(paint.reason?.key, 'backups.snapshot_select_folder_first');
  assertEquals(setActionRefusal(m)?.key, 'backups.snapshot_select_folder_first');
  assert(!setActionPaint(machine([])).disabled);
});

Deno.test('every snapshot control is dead with no reason before the machine exists', () => {
  for (const paint of [setActionPaint(null), deletePaint(null, 0), immediateDeletePaint(null, 0)]) {
    assert(paint.shown && paint.disabled);
    assertEquals(paint.reason, null);
  }
  // A gesture with no machine refuses, with nothing to say.
  assertEquals(setActionRefusal(null), null);
});

// ── Restore ─────────────────────────────────────────────────────────

Deno.test('the restore inputs need a snapshot to restore', () => {
  assertEquals(restoreInputsPaint(faces, null).reason?.key, 'backups.restore_no_snapshots');
  assert(!restoreInputsPaint(faces, '7').disabled);
});

Deno.test('the restore confirm needs its kind ticked and its id re-typed', () => {
  assertEquals(restorePaint(faces, false, '7', false, '7').reason?.key, 'backups.restore_kind_unticked');
  assertEquals(restorePaint(faces, false, '7', true, '8').reason?.key, 'backups.restore_progress_idle');
  assertEquals(restoreRefusal(faces, true, '7', true, '7')?.key, 'backups.restore_progress_running');
  assert(!restorePaint(faces, false, '7', true, '7').disabled);
  assertEquals(restoreRefusal(faces, false, '7', true, '7'), null);
});

// ── The carve-out fetches ───────────────────────────────────────────

Deno.test('the per-file download is blocked while offline', () => {
  const paint = carveOutPaint(faces, 'disconnected');
  assert(paint.shown && paint.disabled);
  assertEquals(paint.reason?.key, 'common.needs_nest');
  assertEquals(carveOutRefusal(faces, 'disconnected')?.key, 'common.needs_nest');
  assert(!carveOutPaint(faces, 'connected').disabled);
  assertEquals(carveOutRefusal(faces, 'connected'), null);
});

// ── The gesture guard ───────────────────────────────────────────────

Deno.test('a refused gesture stops and shows its reason; a possible one runs', () => {
  const m = machine(['pending', 'active']);
  const shown: string[] = [];
  const show = (r: { key: string }) => shown.push(r.key);
  assert(refuse(deleteAnswer(m, 0), show));
  assertEquals(shown, ['backups.snapshot_delete_already_pending']);
  assert(!refuse(deleteAnswer(m, 1), show));
  // Not ready, or absent: stops, with nothing to say.
  assert(refuse(deleteAnswer(null, 0), show));
  assert(refuse(deleteAnswer(m, 9), show));
  assertEquals(shown.length, 1);
});
