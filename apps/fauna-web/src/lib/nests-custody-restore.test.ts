// Deno tests for the Nests page's restore-answer reading. Run via
// `just web-unit-test`. The answer JSON is the shape `accountRestoreFromCustodian`
// sends (`libs/fauna-wasm/src/account_runtime.rs::restore_answer`, pinned
// in-browser there); here, that the SPA paints Rust's parts and authors none.

import { readRestoreAnswer } from './nests-custody-restore.ts';

function expect(cond: boolean, msg: string): void {
  if (!cond) throw new Error(msg);
}

Deno.test('a restored answer joins its parts and closes the form', () => {
  const r = readRestoreAnswer(JSON.stringify({
    outcome: { Restored: { applied: 3, sealed_rows: 2 } },
    notice: [
      { key: 'nests.custody_restore_restored', args: { count: '3' } },
      { key: 'nests.custody_restore_sealed', args: { count: '2' } },
    ],
    closes_form: true,
  }));
  expect(r.kind === 'notice', JSON.stringify(r));
  if (r.kind !== 'notice') return;
  expect(r.closesForm, 'a restored outcome closes the form');
  expect(r.text.includes('Restored 3') && r.text.includes('2 came back sealed'), r.text);
  expect(!r.text.includes('nests.'), `no raw key: ${r.text}`);
});

Deno.test('a revoked answer resolves its nested reason, form stays open', () => {
  const r = readRestoreAnswer(JSON.stringify({
    outcome: 'Revoked',
    notice: [{ key: 'nests.custody_restore_refused', args: { reason: 'nests.custody_restore_revoked' } }],
    closes_form: false,
  }));
  expect(r.kind === 'notice', JSON.stringify(r));
  if (r.kind !== 'notice') return;
  expect(!r.closesForm, 'a refusal keeps the form open');
  expect(r.text.includes('revoked') && !r.text.includes('nests.'), r.text);
});

Deno.test('a failed answer goes to the error surface, not the notice', () => {
  const r = readRestoreAnswer(JSON.stringify({ failed: 'the account runtime is not running' }));
  expect(r.kind === 'failed' && r.reason.includes('runtime'), JSON.stringify(r));
});

Deno.test('a malformed answer is a failure, never a silent drop', () => {
  expect(readRestoreAnswer('not json').kind === 'failed', 'unparseable');
  expect(readRestoreAnswer('{"outcome":"NotAdmitted"}').kind === 'failed', 'no notice parts');
});
