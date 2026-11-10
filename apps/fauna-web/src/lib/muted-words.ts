// The session-held muted-words list — web's twin of tui's
// `SettingsState::muted_words` and android's `MutedWordsHolder`: the one
// in-memory copy every collapse and toast gate reads
// (`docs/goal/behavior/notifications.md` § Muted words → *Where logic lives*).
//
// Loaded at sign-in (`RootShell`), so the knock toast's rule-6 gate holds on a
// fresh tab that never visited a page; re-read by the conversations and
// notifications pages on mount (`loadMutedWords`), and replaced by every
// Settings write's returned snapshot (`holdMutedWords`), so a page and the
// toast gate never read two different lists. Cleared on an actor switch.

import { mutedKeywordsList, type MutedKeyword } from './rpc';

let held: MutedKeyword[] = [];

/** The list as last read or written — what the knock toast's gate asks. */
export function heldMutedWords(): MutedKeyword[] {
  return held;
}

/** Replace the held copy with a snapshot the nest just returned (a Settings
 *  write's reply, or a read). */
export function holdMutedWords(keywords: MutedKeyword[]): void {
  held = keywords;
}

/** Read the record from the nest, hold it, and return it. Throws like the
 *  read does; the held copy is untouched on a failure. */
export async function loadMutedWords(secretHex: string): Promise<MutedKeyword[]> {
  const keywords = (await mutedKeywordsList(secretHex)).keywords;
  held = keywords;
  return keywords;
}

/** Forget the held copy — the actor-switch reset. */
export function clearMutedWords(): void {
  held = [];
}
