import { resolveLocalized, type LocalizedText } from './i18n/localized.ts';

/**
 * The text a caught rejection paints, or `fallback` when it carries none.
 *
 * A wasm export rejects with a plain **string** — the error's `Display`, which
 * for a nest refusal is already `RpcError::localized()` in the user's language
 * (`libs/fauna-wasm/src/rpc.rs::err_to_js`). A catch that reads only
 * `e instanceof Error` therefore threw the nest's reason away and painted its
 * own generic line — the taken-handle refusal read "Failed to change handle"
 * where the user needed "That handle already belongs to someone else"
 * (`settings.md` § User actions: refused "with your nest's reason").
 *
 * A wasm export whose refusal has shared copy rejects with that copy as a
 * `LocalizedText` `{ key, args }` instead (the account switch,
 * `libs/fauna-wasm/src/accounts.rs::activate`); it resolves here like every
 * other wasm `LocalizedText`.
 */
export function rejectionText(e: unknown, fallback: string): string {
  if (typeof e === 'string' && e.length > 0) return e;
  if (e && typeof e === 'object' && typeof (e as LocalizedText).key === 'string') {
    return resolveLocalized(e as LocalizedText);
  }
  if (e instanceof Error && e.message) return e.message;
  return fallback;
}
