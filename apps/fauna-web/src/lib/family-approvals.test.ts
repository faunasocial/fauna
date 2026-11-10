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

import { approvalText, decideApproval, type DecideVerbs } from './family-approvals.ts';
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

// family-safety.md § Guardian policy — a held mail's row reads sender · time
// (envelope metadata); the shared rule (`approvalTimestamp`, over wasm) decides
// which kinds carry a time, so the call site only joins what it returns.
Deno.test('a held mail reads sender · time; no stamp or another kind reads as before', () => {
  const when = (kind: string, createdAt: number): string | null =>
    kind === 'mail_hold' && createdAt > 0 ? '14:03' : null;
  const hold = entry({ kind: 'mail_hold', peer_address: 'stranger@example.com', created_at: 1_700_000_000 });
  eq(approvalText(hold, fakeRaw, when), 'stranger@example.com · 14:03', 'stamped hold');
  eq(
    approvalText({ ...hold, created_at: 0 }, fakeRaw, when),
    'stranger@example.com',
    'unstamped hold',
  );
  eq(
    approvalText({ ...hold, peer_address: '' }, fakeRaw, when),
    `${t.family.approval_no_sender} · 14:03`,
    'null-path hold',
  );
  const contact = entry({ kind: 'contact', summary: 'hi from bob', created_at: 1_700_000_000 });
  eq(approvalText(contact, fakeRaw, when), 'hi from bob', 'contact row');
});

// ── The family composer (account-offline-mutation.md § Implementation status
// today): `decideApproval`'s routing — the composer's send-or-queue and the
// shared fold are faked here; both are pinned Rust-side
// (`fauna-client-family`'s `a_decision_with_no_nest_is_queued_scoped_to_its_item`
// and `a_queued_decision_drops_only_its_own_item`).

function recorder(queued: boolean) {
  const calls: string[] = [];
  const verbs: DecideVerbs = {
    decideOrQueue: (e, approve) => {
      calls.push(`decide:${e.kind}:${approve}`);
      return Promise.resolve(queued);
    },
    afterQueued: (list, decided) => {
      calls.push('fold');
      return list.filter((e) => e !== decided);
    },
    reread: () => {
      calls.push('reread');
      return Promise.resolve();
    },
  };
  return { calls, verbs };
}

Deno.test('a decision with no nest is queued and leaves the local list, nothing re-read', async () => {
  const held = entry({ kind: 'mail_hold', message_id: new Uint8Array(32).fill(9) });
  const other = entry({ kind: 'contact' });
  const { calls, verbs } = recorder(true);
  const out = await decideApproval(verbs, [held, other], held, true);
  eq(out?.length, 1, 'the decided item left the list');
  eq(out?.[0], other, 'the other item stays');
  eq(calls.join(','), 'decide:mail_hold:true,fold', 'queued: folded, never re-read');
});

Deno.test('a decision with the nest connected is sent and the page re-reads', async () => {
  const held = entry({ kind: 'mail_hold' });
  const { calls, verbs } = recorder(false);
  const out = await decideApproval(verbs, [held], held, false);
  eq(out, null, 'the re-read sets the list');
  eq(calls.join(','), 'decide:mail_hold:false,reread', 'sent: re-read, never folded');
});
