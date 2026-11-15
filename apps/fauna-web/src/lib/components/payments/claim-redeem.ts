// The one decision both claim-redeem doors share (`ClaimRedeem.svelte`: the
// subscription-settings pair and the OTHER profile Tiers tab's pair;
// `monetization.md` § Pillar 3 → *Which nest an in-app redeem binds on* → *The
// doors*): a blank (trimmed) code or a not-yet-resolved identity is a no-op —
// never a nest round-trip — and anything else is ONE across-nests redeem of the
// trimmed code under the door's author + home. Kept free of imports so the
// Deno unit tests drive it with a stand-in for the wasm face.

/** Where a door's code redeems: the creator it names and that creator's home
 *  nest (`null` = this nest, where the author slot is never read). */
export interface ClaimRedeemTarget {
  authorIdHex: string;
  homeNestUrl: string | null;
}

/** The across-nests face's shape (`paymentsClaimsRedeemAcrossNests`). */
export type ClaimRedeemCall<R> = (
  secretHex: string,
  code: string,
  authorIdHex: string,
  homeNestUrl: string | null,
) => Promise<R>;

/** Redeem `rawCode` at `target`, or answer `null` without calling when there is
 *  nothing to send. A refusal rejects, exactly as the face does. */
export async function redeemClaimCode<R>(
  rawCode: string,
  secretHex: string | null,
  target: ClaimRedeemTarget,
  call: ClaimRedeemCall<R>,
): Promise<R | null> {
  const code = rawCode.trim();
  if (!secretHex || !code) return null;
  return await call(secretHex, code, target.authorIdHex, target.homeNestUrl);
}
