// Deno tests for the `family-approval-item` display-text rule. Run via:
//
//     deno test apps/fauna-web/src/lib/family-approvals.test.ts
//
// `approvalText`'s own logic is just the localized no-sender fallback over
// whatever `raw` decides — the per-kind field decision itself is
// `fauna_core::format::approval_display_text`, pinned Rust-side
// (`libs/fauna-core/src/format.rs::approval_display_text_tests`). `fakeRaw`
// here mirrors that fn's exact contract so these tests exercise the SAME
// cases end-to-end (family-safety.md § Reach approvals) without
// loading wasm/`$app/paths` — see `family-approvals.ts`'s doc comment on why
// `raw` has no default.

import { approvalText } from './family-approvals.ts';
import { t } from './i18n/strings.ts';
import type { FamilyApprovalEntry } from './rpc.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${JSON.stringify(actual)}, want ${JSON.stringify(expected)}`);
  }
}

function entry(partial: Partial<FamilyApprovalEntry>): FamilyApprovalEntry {
  return {
    supervised_actor_id: new Uint8Array(32),
    supervised_handle: 'kid',
    kind: 'contact',
    peer_actor_id: new Uint8Array(32),
    peer_address: '',
    message_id: new Uint8Array(0),
    summary: '',
    created_at: 0,
    ...partial,
  };
}

// Mirrors `fauna_core::format::approval_display_text`'s exact contract.
function fakeRaw(
  kind: string,
  peerAddress: string,
  peerHandle: string,
  summary: string,
  bridgeId: string,
  operation: string,
  target: string,
): string | null {
  if (kind === 'feed_source') {
    const key = [bridgeId, operation, target].filter((p) => p !== '');
    if (key.length === 0) return null;
    return key.join(' · ') + (summary === '' ? '' : ` — “${summary}”`);
  }
  const text = kind === 'mail_hold' || kind === 'dm_hold'
    ? peerAddress
    : kind === 'contact_request'
    ? peerHandle
    : summary;
  return text === '' ? null : text;
}

Deno.test('a mail_hold renders its peer_address, not the empty summary', () => {
  const hold = entry({ kind: 'mail_hold', peer_address: 'stranger@example.com' });
  eq(approvalText(hold, fakeRaw), 'stranger@example.com', 'mail_hold row text');
});

Deno.test('a contact renders its summary', () => {
  const contact = entry({ kind: 'contact', summary: 'hi from bob' });
  eq(approvalText(contact, fakeRaw), 'hi from bob', 'contact row text');
});

Deno.test('a null-path mail_hold renders the localized no-sender label', () => {
  const nullPathHold = entry({ kind: 'mail_hold', peer_address: '' });
  eq(approvalText(nullPathHold, fakeRaw), t.family.approval_no_sender, 'null-path row text');
});

// family-safety.md § Child-initiated contact requests — the ask carries no
// message text at all ("who, never why"), so binding `summary` renders a blank
// row beside live Approve/Deny buttons.
Deno.test('a contact_request renders its peer_handle, not the empty summary', () => {
  const ask = entry({ kind: 'contact_request', peer_handle: 'alice' });
  eq(approvalText(ask, fakeRaw), 'alice', 'contact_request row text');
});

// family-safety.md § The bridge-DM gate — the external peer id rides
// `peer_address`; the message itself is sealed to the ward, so `summary` is
// always empty.
Deno.test('a dm_hold renders its peer_address, not the empty summary', () => {
  const held = entry({ kind: 'dm_hold', peer_address: 'npub1stranger' });
  eq(approvalText(held, fakeRaw), 'npub1stranger', 'dm_hold row text');
});

// The generalized invariant: no kind may reach the row blank.
Deno.test('every kind falls back to the localized label rather than a blank row', () => {
  for (const kind of ['mail_hold', 'dm_hold', 'contact_request', 'contact', 'feed_source']) {
    eq(approvalText(entry({ kind }), fakeRaw), t.family.approval_no_sender, `${kind} row text`);
  }
});

// family-safety.md § Feed-source approvals — the grant matches
// `(bridge_id, operation, target)`, never the label, so the card names the
// target the Approve button grants and quotes the ward's label after it. This
// pins that the web call site passes the whole key through, not just `summary`.
Deno.test('a feed_source renders the grant key, not only the ward label', () => {
  const ask = entry({
    kind: 'feed_source',
    summary: "Grandma's photos",
    bridge_id: 'bluesky',
    operation: 'follow',
    target: 'did:plc:somethingelse',
  });
  eq(
    approvalText(ask, fakeRaw),
    "bluesky · follow · did:plc:somethingelse — “Grandma's photos”",
    'feed_source row text',
  );
});
