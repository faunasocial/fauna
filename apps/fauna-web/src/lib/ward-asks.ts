// The supervised ward's in-place asks and the guardian's un-deny list — the
// pure decision rules behind four family-safety surfaces
// (family-safety.md § Child-initiated contact requests → App affordance,
// § Feed-source approvals, § The bridge-DM gate → The un-deny surface):
//
//   * `contact-request-guardian-button` / `contact-request-pending` on the
//     contacts page's Find User result and on another actor's profile;
//   * `bridge-source-request-button` / `bridge-source-request-state` inside a
//     `bridge-card[i]`;
//   * `family-blocked-peer-item` / `family-blocked-peer-allow-button` in the
//     guardian's per-ward editor.
//
// The web lift of tui's reference implementation (`apps/fauna-tui/src/
// {contacts,profile/mod,bridges,family}.rs`), whose rules every app copies
// rather than re-derives:
//   (a) the ask is offered ONLY on the typed refusal (`guardian-refusal.ts`);
//   (b) the refusal stays on `error-message` — the page's job, not this module's;
//   (c) pending is durable — read from `status.contact_requests` /
//       `status.feed_requests`, gated on `supervised_by`; a just-asked flag only
//       makes the render answer before the re-read lands;
//   (d) rows are keyed on the ask data, never on compose buffers;
//   (e) an approved feed-source ask is a PROMPT to retry, never an auto-retry;
//   (f) each allow button addresses ITS OWN row's (bridge_id, peer_id);
//   (g) a knock reply carries the peer it was sent to, and paints only on that
//       peer's open;
//   (h) supervision is not re-tested at render — the inputs are
//       supervised-only by construction.
//
// Plain TS with NO `$lib`/wasm import so it stays plain-Deno-testable
// (`ward-asks.test.ts`); the wire shapes are restated structurally here
// (they are the `fauna_protocol::family` serde forms `$lib/rpc` also types).
// The two ask-matching rules are NOT restated: they are the shared
// `fauna_client_family::ward_asks::{contact_ask_pending, feed_request_state}`,
// injected as an `AskRules` — `$lib/wasm`'s `wardAskRules` in the app (their
// wasm faces), a stub in the tests.

import { isGuardianApprovalRequired } from './guardian-refusal.ts';

/** A raw 32-byte id as serde_wasm_bindgen hands it over (either shape). */
export type WireBytes = Uint8Array | number[];

/** One of the ward's own outstanding contact asks (`status.contact_requests`). */
export interface ContactAsk {
  peer_actor_id: WireBytes;
  peer_handle?: string;
  created_at: number;
}

/** One of the ward's own live feed-source asks (`status.feed_requests`) —
 *  pending while `approved_at` is absent, an approved single-use grant once set. */
export interface FeedAsk {
  bridge_id: string;
  /** `link` | `follow` | `feed`. */
  operation: string;
  /** Empty for a `link`; the follow id / feed URI otherwise. */
  target: string;
  label?: string;
  created_at: number;
  approved_at?: number | null;
}

/** One guardian-denied bridge-DM peer on a ward (`FamilyWardInfo.blocked_dm_peers`). */
export interface BlockedPeer {
  bridge_id: string;
  peer_id: string;
}

/** The ward's own asks, off one `fauna.family.status` read. */
export interface WardAsks {
  contact: ContactAsk[];
  feed: FeedAsk[];
}

export const NO_WARD_ASKS: WardAsks = { contact: [], feed: [] };

/** Rule (c)/(h): the asks a status reply carries, gated on `supervised_by`. A
 *  graduated account has no guardian to be waiting on, so a stale row from the
 *  last read must not keep painting "asked — waiting" (or a "try again"
 *  prompt) on a page that now acts freely. The nest drops the rows at
 *  graduation too; this is the client half of the same rule. */
export function wardAsksFromStatus(status: {
  supervised_by?: unknown;
  contact_requests?: ContactAsk[] | null;
  feed_requests?: FeedAsk[] | null;
}): WardAsks {
  if (status.supervised_by == null) return { contact: [], feed: [] };
  return {
    contact: status.contact_requests ?? [],
    feed: status.feed_requests ?? [],
  };
}

/** A failed re-read after an ask is NOT a failed ask — the guardian has been
 *  rung either way — so an empty re-read keeps what the client already holds
 *  rather than wiping it. */
export function keepOnEmptyReread<T>(held: T[], reread: T[]): T[] {
  return reread.length > 0 ? reread : held;
}

/** The two live states a feed-source ask can be in. */
export type FeedRequestState = 'pending' | 'approved';

/** The shared Rust ask-matching rules (`fauna_client_family::ward_asks`),
 *  injected so this module stays plain-Deno-testable. */
export interface AskRules {
  /** Whether `asks` holds an outstanding contact ask for `peerHex`
   *  (case-insensitive; a non-hex id matches nothing). */
  contactAskPending(asks: ContactAsk[], peerHex: string): boolean;
  /** The live ask state for one `(bridge_id, operation, target)` triple, or
   *  `null` when no live ask covers it. */
  feedRequestState(
    asks: FeedAsk[],
    bridgeId: string,
    operation: string,
    target: string,
  ): FeedRequestState | null;
}

// ── Contact ask (contacts page + profile page) ─────────────────────────────

/** What the refused-send surface shows beside the knock: the durable pending
 *  label (read FIRST — it survives navigation and a restart, and is honest on
 *  a fresh session that never saw the refusal), else the ask button only after
 *  a TYPED refusal this open saw, else nothing. */
export type ContactAskRender = 'pending' | 'ask' | null;

export function contactAskRender(
  rules: AskRules,
  asks: ContactAsk[],
  peerHex: string,
  guardianRefused: boolean,
  askSent: boolean,
): ContactAskRender {
  if (rules.contactAskPending(asks, peerHex) || askSent) return 'pending';
  if (guardianRefused) return 'ask';
  return null;
}

/** The per-peer knock state a page holds for the peer it currently shows.
 *  `peer` names whom the flags belong to, so a new lookup / a new profile open
 *  resets them instead of carrying a refusal over to somebody else. */
export interface KnockAskState {
  peer: string | null;
  knockSent: boolean;
  guardianRefused: boolean;
  askSent: boolean;
}

/** A fresh state for `peer` — a new lookup, or a new profile open. The refusal
 *  and the ask belong to the peer they were made for; carrying either across
 *  would offer to ask the guardian about somebody the ward did not name. */
export function knockStateFor(peer: string | null): KnockAskState {
  return { peer, knockSent: false, guardianRefused: false, askSent: false };
}

/** How a knock send ended, classified once: the one failure that reveals the
 *  ask (the TYPED guardian refusal — rule (a)) apart from every other one. */
export type KnockSendResult =
  | { kind: 'sent' }
  | { kind: 'refused_by_guardian' }
  | { kind: 'failed'; error: unknown };

/** A knock send that landed. */
export const KNOCK_SENT: KnockSendResult = { kind: 'sent' };

/** Classify a rejected knock send (a rejected wasm call's value). */
export function classifyKnockFailure(error: unknown): KnockSendResult {
  return isGuardianApprovalRequired(error)
    ? { kind: 'refused_by_guardian' }
    : { kind: 'failed', error };
}

/** What `error-message` should do after a knock reply: clear it, show the
 *  localized guardian-gate sentence (rule (b) — still a real failure, just no
 *  longer a dead end), or show another failure's own text. */
export type KnockErrorEffect =
  | { kind: 'clear' }
  | { kind: 'guardian' }
  | { kind: 'failed'; error: unknown };

/** Rule (g): fold a knock reply sent to `sentTo` into the state of the page
 *  as it is NOW. The reply can outlive the open it was sent from — a "Sent" or
 *  a guardian refusal for the actor you just left must not paint on the one you
 *  are now viewing — so the flags move only when `state.peer === sentTo`. The
 *  error effect applies regardless: the send genuinely failed (or landed). */
export function foldKnockReply(
  state: KnockAskState,
  sentTo: string,
  result: KnockSendResult,
): { state: KnockAskState; error: KnockErrorEffect } {
  const current = state.peer === sentTo;
  switch (result.kind) {
    case 'sent':
      return {
        state: current ? { ...state, knockSent: true } : state,
        error: { kind: 'clear' },
      };
    case 'refused_by_guardian':
      return {
        state: current ? { ...state, guardianRefused: true } : state,
        error: { kind: 'guardian' },
      };
    case 'failed':
      return { state, error: { kind: 'failed', error: result.error } };
  }
}

/** The guardian ask for `askedFor` landed: flip the just-asked flag only on
 *  that peer's own open (rule (g), the ask half). */
export function foldContactAsked(state: KnockAskState, askedFor: string): KnockAskState {
  return state.peer === askedFor ? { ...state, askSent: true } : state;
}

// ── Feed-source ask (bridges page) ─────────────────────────────────────────

/** The `(bridge_id, operation, target)` triple a feed-source grant is scoped to. */
export interface FeedTriple {
  bridge_id: string;
  operation: string;
  target: string;
}

function sameTriple(a: FeedTriple, b: FeedTriple): boolean {
  return a.bridge_id === b.bridge_id && a.operation === b.operation && a.target === b.target;
}

/** Record a triple the guardian gate just refused (a set: re-refusing the
 *  same operation adds nothing). */
export function addRefusedTriple(refused: FeedTriple[], t: FeedTriple): FeedTriple[] {
  return refused.some((r) => sameTriple(r, t)) ? refused : [...refused, t];
}

/** What one bridge card paints for the ask surface. */
export interface SourceAskRows {
  /** One `bridge-source-request-state` label per durable ask on this bridge. */
  states: FeedRequestState[];
  /** One `bridge-source-request-button` per refused triple this session saw
   *  that has no durable row yet — each carrying its own triple. */
  asks: FeedTriple[];
}

/** The card's ask rows. Paints nothing in the common case: both inputs are
 *  supervised-only by construction (rule (h)). Durable rows first, then
 *  session-only refusals; a triple with a durable row is skipped in the second
 *  pass, so an answered ask shows its verdict instead of offering the button
 *  again. An `approved` row is a LABEL (the "try again" prompt), never a
 *  button — the ward redeems the grant by retrying the ORIGINAL gesture
 *  (rule (e)). Keyed on the ask data, never the add-form buffers, which the
 *  follow form clears at dispatch (rule (d)). */
export function sourceAskRows(
  rules: AskRules,
  feedAsks: FeedAsk[],
  refused: FeedTriple[],
  bridgeId: string,
): SourceAskRows {
  const states = feedAsks
    .filter((a) => a.bridge_id === bridgeId)
    .map((a) => rules.feedRequestState([a], a.bridge_id, a.operation, a.target))
    .filter((st): st is FeedRequestState => st !== null);
  const asks = refused.filter(
    (r) =>
      r.bridge_id === bridgeId &&
      rules.feedRequestState(feedAsks, r.bridge_id, r.operation, r.target) === null,
  );
  return { states, asks };
}

// ── The un-deny surface (family page, guardian side) ───────────────────────

/** One `family-blocked-peer-item` row: its text (the peer id — the only name
 *  this nest has for an external bridge peer) and the peer ITS OWN allow
 *  button un-denies, handed as-is to the shared un-deny
 *  (`familyAllowBlockedDmPeer`, which owns the approving `dm_hold` decide's
 *  wire shape — idempotent and not queue-scoped, so it works long after the
 *  hold row that prompted the deny is gone). Rule (f): each row carries its
 *  own (bridge_id, peer_id); pointing every button at row 0 would un-deny the
 *  wrong person while the surface still looked correct. */
export interface BlockedPeerRow {
  text: string;
  peer: BlockedPeer;
}

export function blockedPeerRows(peers: BlockedPeer[] | null | undefined): BlockedPeerRow[] {
  return (peers ?? []).map((p) => ({
    text: p.peer_id,
    peer: { bridge_id: p.bridge_id, peer_id: p.peer_id },
  }));
}
