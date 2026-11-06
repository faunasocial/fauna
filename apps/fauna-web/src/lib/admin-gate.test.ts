// Deno test for `applyAdminGateRead` — the admin gate's reconnect re-read rule
// (`transport.md` § Which surfaces a push invalidates, `admin_gate`). Run via:
//
//     deno test apps/fauna-web/src/lib/admin-gate.test.ts

import { applyAdminGateRead } from './admin-gate.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) throw new Error(`${msg}\n  expected: ${expected}\n  actual:   ${actual}`);
}

Deno.test('a failed re-read keeps a confirmed admin and fires no effect', () => {
  const r = applyAdminGateRead(true, null);
  eq(r.gate, true, 'gate kept');
  eq(r.becameAdmin, false, 'no admin effect');
});

Deno.test('a failed re-read leaves a non-admin hidden', () => {
  const r = applyAdminGateRead(false, null);
  eq(r.gate, false, 'gate kept');
  eq(r.becameAdmin, false, 'no admin effect');
});

Deno.test('a confirming re-read does not re-fire the admin effects', () => {
  const r = applyAdminGateRead(true, true);
  eq(r.gate, true, 'gate stays');
  eq(r.becameAdmin, false, 'no re-fire');
});

Deno.test('the false-to-true transition fires the admin effects once', () => {
  const r = applyAdminGateRead(false, true);
  eq(r.gate, true, 'gate opens');
  eq(r.becameAdmin, true, 'effects fire');
});

Deno.test('a successful "not an admin" answer closes the gate', () => {
  const r = applyAdminGateRead(true, false);
  eq(r.gate, false, 'gate closes');
  eq(r.becameAdmin, false, 'no admin effect');
});
