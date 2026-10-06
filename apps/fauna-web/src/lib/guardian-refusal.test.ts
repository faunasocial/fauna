// Deno tests for the guardian-refusal reader. Run via:
//
//     deno test apps/fauna-web/src/lib/guardian-refusal.test.ts

import { assertEquals } from 'jsr:@std/assert@1';
import { isGuardianApprovalRequired, refusalText } from './guardian-refusal.ts';

Deno.test('the wasm-prefixed guardian refusal is recognised, as a string or an Error', () => {
  const raw = 'guardian_approval_required: This account can only message approved contacts.';
  assertEquals(isGuardianApprovalRequired(raw), true);
  assertEquals(isGuardianApprovalRequired(new Error(raw)), true);
  assertEquals(refusalText(raw), 'This account can only message approved contacts.');
});

Deno.test('any other failure is not the guardian refusal, and keeps its text', () => {
  for (const raw of ['ws connect: refused', 'transient: timeout', new Error('boom'), null]) {
    assertEquals(isGuardianApprovalRequired(raw), false);
  }
  assertEquals(refusalText('ws connect: refused'), 'ws connect: refused');
});

Deno.test('the prefix is matched at the start only', () => {
  assertEquals(isGuardianApprovalRequired('boom: guardian_approval_required: x'), false);
});
