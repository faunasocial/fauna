/**
 * NIP-23 long-form article Markdown → HTML.
 *
 * The parser and HTML renderer now live in shared Rust (`fauna_core::markdown`, reached
 * over wasm) so web, android, and windows all render the same Markdown subset instead of
 * each maintaining a divergent converter (priority #1/#2/#4). See
 * `docs/goal/ui/feed.md` § Article. The native apps consume the same parser's token
 * model over UniFFI (`fauna-ffi` `parse_markdown`).
 */
export { markdownToHtml } from './wasm';
