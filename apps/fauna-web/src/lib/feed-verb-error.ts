// A rejected feed verb as the page's `error-message` paints it: a **stated
// refusal** (words under a restricted post — `docs/goal/behavior/restricted-posts.md`
// ruling 6) reads in the user's language through the shared `refusal_i18n_key`;
// every other failure keeps its own text. The twin of tui's `refusal_copy`.
//
// The recognizer is a parameter (the page passes the wasm `feedRefusalI18nKey`)
// so a plain `deno test` pins the mapping without loading wasm. Relative import
// for the same reason as `i18n/localized.ts`.
import { resolveKey } from './i18n/localized.ts';

export function verbErrorCopy(
  e: unknown,
  refusalKey: (text: string) => string | null | undefined,
): string {
  const text = e instanceof Error ? e.message : String(e);
  const key = refusalKey(text);
  return key ? resolveKey(key) : text;
}
