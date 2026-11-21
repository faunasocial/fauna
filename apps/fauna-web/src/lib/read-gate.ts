// The read-state half of an affordance's answer (`docs/goal/ui/README.md`
// § Offer only what is possible, rules 2, 5, 6; `docs/goal/ui/profile.md`
// § Errors & edge cases, *Unread state is not possible*;
// `docs/goal/behavior/notifications.md` § Errors & edge cases, *The page-wide
// actions wait on the first read*).
//
// Unknown is not possible: before the read an affordance waits on answers, the
// affordance is blocked still loading; after a failed read, blocked with the
// failure. The answers themselves are shared Rust over wasm (`#lib/wasm`'s
// `readPossibility`, `featuresGatePossibility`,
// `notifications*Possibility`, `WasmOverlayEditor.savePossibility`); this
// module only tracks which word the page's read stands at and picks which
// shared answer to ask. The asks are handed in, so plain `deno test` reaches
// this module without the wasm core (`read-gate.test.ts`); the tui twins are
// `apps/fauna-tui/src/profile/mod.rs`'s `follow_answer` / `block_answer`,
// `notifications.rs`'s `mark_read_answer` / `clear_answer` and `nostr.rs`'s
// `zap_signer_add_answer`.

import type { Possibility } from './possibility.ts';

/** Where a page's read stands — the word the wasm read answers take
 *  (`fauna_core::possibility::ReadState`). */
export type ReadWord = 'pending' | 'failed' | 'resolved';

/** The read word after a read came back with an error: a read that has
 *  answered once stays answered (the rows already shown stay actionable);
 *  otherwise it failed. */
export function readFailed(prev: ReadWord): ReadWord {
  return prev === 'resolved' ? 'resolved' : 'failed';
}

/** `fauna_core::possibility::Possibility::and` — absent wins, then the first
 *  blocked reason, else the second answer. Composes two shared answers (the
 *  page's gate first, then the draft); it decides no rule of its own. */
export function andPossibility(first: Possibility, second: Possibility): Possibility {
  if (first.state === 'absent' || second.state === 'absent') return { state: 'absent' };
  if (first.state === 'blocked') return first;
  return second;
}

/** `profile-follow-button`: possible once the page knows which way the press
 *  goes — the viewer pressed it this open, or the open-time status read
 *  answered; before that still loading, after a failed read the failure. */
export function followAnswer(
  readPossibility: (read: ReadWord) => Possibility,
  pressedThisOpen: boolean,
  statusRead: ReadWord,
): Possibility {
  return readPossibility(pressedThisOpen ? 'resolved' : statusRead);
}

/** `profile-block-button`: possible once the open-time block read (or the
 *  viewer's own press) set the edge; blocked `common.still_loading` before it
 *  and `common.load_failed` after a failed read (`Possibility::from_read`). */
export function blockAnswer(
  readPossibility: (read: ReadWord) => Possibility,
  read: ReadWord,
): Possibility {
  return readPossibility(read);
}

/** A gated feature's own affordance (the zap-signer designate, the claim
 *  redeem): the shared features gate over the rows the page holds. With no
 *  rows yet the gate answers still loading; a read that failed with no rows in
 *  hand answers the failure instead. */
export function featureAnswer<R>(
  asks: {
    readPossibility: (read: ReadWord) => Possibility;
    featuresGatePossibility: (rows: R[] | null, feature: string) => Possibility;
  },
  rows: R[] | null,
  read: ReadWord,
  feature: string,
): Possibility {
  if (rows === null && read === 'failed') return asks.readPossibility('failed');
  return asks.featuresGatePossibility(rows, feature);
}
