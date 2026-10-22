// Which shape a DM bubble paints an admin block's notice in (`moderation.md` § Admin block of a
// reported item, decision 4). Kept free of `#lib` imports so the deno test can load it bare.

/**
 * `'in-place'` for a withheld record (an empty document — the Rust
 * `MessageSnapshot::admin_block_in_place`), `'beside'` for the author's own blocked message
 * (the body is kept, the notice paints right after it), `'none'` when unblocked.
 */
export function adminBlockPaint(
  reason: string | null | undefined,
  doc: { blocks?: unknown[] } | undefined,
): 'none' | 'in-place' | 'beside' {
  if (!reason) return 'none';
  return !doc || !doc.blocks || doc.blocks.length === 0 ? 'in-place' : 'beside';
}

/**
 * Whether a `dm-message-withheld-badge` already says the admin-block notice — the Rust
 * `WithheldOwnMessage::carries_block_notice` (a `Blocked` ground; serde spells it `"Blocked"`).
 * The bubble then paints the badge in place of the beside-body notice, so it says it once.
 */
export function withheldCarriesBlockNotice(
  withheld: { ground?: string } | null | undefined,
): boolean {
  return withheld?.ground === 'Blocked';
}
