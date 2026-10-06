// Deno tests for the ward-ask / un-deny rules — the web twins of tui's unit
// arms (`apps/fauna-tui/src/{contacts,profile/mod,bridges,family}.rs`). Run via:
//
//     deno test apps/fauna-web/src/lib/ward-asks.test.ts

import { assert, assertEquals } from 'jsr:@std/assert@1';
import {
  addRefusedTriple,
  blockedPeerRows,
  classifyKnockFailure,
  contactAskRender,
  foldContactAsked,
  foldKnockReply,
  keepOnEmptyReread,
  KNOCK_SENT,
  knockStateFor,
  sourceAskRows,
  wardAsksFromStatus,
  type AskRules,
  type ContactAsk,
  type FeedAsk,
} from './ward-asks.ts';

const GUARDIAN_REFUSAL = 'guardian_approval_required: This account can only message approved contacts.';

function anAsk(byte: number): ContactAsk {
  return { peer_actor_id: new Array(32).fill(byte), peer_handle: '', created_at: 0 };
}

// The shared rules are Rust-tested (`fauna_client_family::ward_asks`) and
// reach the app through `$lib/wasm`'s `wardAskRules`; this stub is only what
// these render tests need from them — an exact peer / triple match.
const RULES: AskRules = {
  contactAskPending: (asks, peerHex) =>
    asks.some((a) => Array.from(a.peer_actor_id).every((b, i) => b === parseInt(peerHex.slice(i * 2, i * 2 + 2), 16))),
  feedRequestState: (asks, bridgeId, operation, target) => {
    const a = asks.find((x) => x.bridge_id === bridgeId && x.operation === operation && x.target === target);
    return a ? (a.approved_at != null ? 'approved' : 'pending') : null;
  },
};

function aFeedAsk(bridge: string, op: string, target: string, approved: boolean): FeedAsk {
  return { bridge_id: bridge, operation: op, target, label: '', created_at: 1, approved_at: approved ? 2 : null };
}

// ── (c)/(h) the durable lists, gated on supervised_by ──────────────────────

Deno.test('a status reply without supervised_by yields no asks, however many rows it carries', () => {
  const asks = wardAsksFromStatus({
    supervised_by: null,
    contact_requests: [anAsk(0xcd)],
    feed_requests: [aFeedAsk('activitypub', 'follow', 'x', true)],
  });
  assertEquals(asks, { contact: [], feed: [] }, 'a graduated account has no guardian to be waiting on');
});

Deno.test('a supervised reply carries its own asks', () => {
  const asks = wardAsksFromStatus({
    supervised_by: { handle: 'mum' },
    contact_requests: [anAsk(0xcd)],
  });
  assertEquals(asks.contact.length, 1);
  assertEquals(asks.feed, [], 'an absent feed_requests reads as none');
});

Deno.test('an empty re-read after an ask keeps what the client already holds', () => {
  const held = [anAsk(0xab)];
  assertEquals(keepOnEmptyReread(held, []), held);
  const fresh = [anAsk(0xcd)];
  assertEquals(keepOnEmptyReread(held, fresh), fresh);
});

// ── the contact-ask render (contacts page + profile page) ──────────────────

Deno.test('a guardian-refused knock offers the ask, then shows it pending', () => {
  const peer = 'cd'.repeat(32);
  let st = knockStateFor(peer);
  assertEquals(contactAskRender(RULES, [], peer, st.guardianRefused, st.askSent), null, 'a clean resolve offers neither');

  // An ordinary failure stays an ordinary failure (rule (a)).
  let r = foldKnockReply(st, peer, classifyKnockFailure(new Error('inbox send: boom')));
  st = r.state;
  assertEquals(r.error.kind, 'failed');
  assertEquals(contactAskRender(RULES, [], peer, st.guardianRefused, st.askSent), null, 'a transport failure must not imply supervision');

  r = foldKnockReply(st, peer, classifyKnockFailure(GUARDIAN_REFUSAL));
  st = r.state;
  assertEquals(r.error.kind, 'guardian', 'the refusal stays on error-message (rule (b))');
  assertEquals(contactAskRender(RULES, [], peer, st.guardianRefused, st.askSent), 'ask');

  st = foldContactAsked(st, peer);
  assertEquals(contactAskRender(RULES, [], peer, st.guardianRefused, st.askSent), 'pending', 'the ask is sent — offering it again would re-ask');
});

Deno.test('a pending ask from the status read renders without this session asking, keyed on the peer', () => {
  const asks = [anAsk(0xcd)];
  assertEquals(contactAskRender(RULES, asks, 'cd'.repeat(32), false, false), 'pending');
  assertEquals(contactAskRender(RULES, asks, 'ab'.repeat(32), false, false), null);
});

Deno.test('a new lookup clears the refusal and the pending flag', () => {
  const peer = 'cd'.repeat(32);
  let st = foldKnockReply(knockStateFor(peer), peer, classifyKnockFailure(GUARDIAN_REFUSAL)).state;
  st = foldContactAsked(st, peer);
  assert(st.guardianRefused && st.askSent);
  const other = 'ab'.repeat(32);
  st = knockStateFor(other);
  assertEquals(contactAskRender(RULES, [anAsk(0xcd)], other, st.guardianRefused, st.askSent), null);
});

// ── (g) a knock reply that outlives its open ──────────────────────────────

Deno.test('a knock reply for a profile you have since left does not paint on the current one', () => {
  const left = 'cd'.repeat(32);
  const now = 'ab'.repeat(32);
  const current = knockStateFor(now);

  const sent = foldKnockReply(current, left, KNOCK_SENT);
  assertEquals(sent.state.knockSent, false, 'a stale "Sent" must not paint on the next actor');

  const refused = foldKnockReply(current, left, classifyKnockFailure(GUARDIAN_REFUSAL));
  assertEquals(refused.state.guardianRefused, false, 'a stale refusal must not offer the ask for the next actor');

  assertEquals(foldContactAsked(current, left).askSent, false, 'a stale ask ack must not paint pending');

  // …while the same replies DO paint on their own open.
  assert(foldKnockReply(knockStateFor(left), left, KNOCK_SENT).state.knockSent);
});

// ── feed-source asks (bridges page) ────────────────────────────────────────

Deno.test('no refusal and no ask renders no source elements', () => {
  assertEquals(sourceAskRows(RULES, [], [], 'activitypub'), { states: [], asks: [] });
});

Deno.test('a guardian refusal offers the ask for the refused triple only', () => {
  const refused = addRefusedTriple([], { bridge_id: 'activitypub', operation: 'follow', target: 'npub1abc' });
  assertEquals(addRefusedTriple(refused, refused[0]).length, 1, 're-refusing adds nothing');
  const rows = sourceAskRows(RULES, [], refused, 'activitypub');
  assertEquals(rows.asks, refused);
  assertEquals(sourceAskRows(RULES, [], refused, 'other').asks, [], 'another card offers nothing');
});

Deno.test('a landed ask replaces the button with its state', () => {
  const refused = [{ bridge_id: 'activitypub', operation: 'follow', target: 'npub1abc' }];
  const rows = sourceAskRows(RULES, [aFeedAsk('activitypub', 'follow', 'npub1abc', false)], refused, 'activitypub');
  assertEquals(rows.asks, [], 'a triple with a durable ask must stop offering the button');
  assertEquals(rows.states, ['pending']);
});

Deno.test('an approved ask prompts the retry and never offers the ask again', () => {
  const rows = sourceAskRows(RULES, [aFeedAsk('activitypub', 'follow', 'npub1abc', true)], [], 'activitypub');
  assertEquals(rows.states, ['approved']);
  assertEquals(rows.asks, []);
});

// ── (f) the un-deny surface ────────────────────────────────────────────────

Deno.test('a ward with nothing denied has no rows', () => {
  assertEquals(blockedPeerRows([]), []);
  assertEquals(blockedPeerRows(undefined), [], 'an omitted field reads as empty');
});

Deno.test('each allow button addresses its own row\'s peer', () => {
  const rows = blockedPeerRows([
    { bridge_id: 'nostr', peer_id: 'npub1aaa' },
    { bridge_id: 'bsky', peer_id: 'npub1bbb' },
  ]);
  assertEquals(rows.map((r) => r.text), ['npub1aaa', 'npub1bbb'], 'the row text IS the peer id');
  assertEquals(
    rows.map((r) => [r.peer.bridge_id, r.peer.peer_id]),
    [['nostr', 'npub1aaa'], ['bsky', 'npub1bbb']],
    'each button must carry its own row\'s (bridge, peer), not the first row\'s',
  );
});
