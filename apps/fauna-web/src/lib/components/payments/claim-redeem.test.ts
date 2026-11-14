// The two claim-redeem doors (`monetization.md` § Pillar 3 → *Which nest an
// in-app redeem binds on* → *The doors*; `profile.md` § Layout & flow →
// *Another's profile*): one across-nests call shape, the profile's carrying the
// viewed creator and the page's actor-home slot, the settings pair's carrying
// no home. The decision runs for real over `redeemClaimCode` with a stand-in
// for the wasm face; the wiring of the two hosts — which this Deno environment
// cannot mount — is asserted over their source text (testing.md convention 17),
// comments stripped first.

import { assert, assertEquals } from "jsr:@std/assert";
import { stripComments } from "../../source-contract.ts";
import { type ClaimRedeemTarget, redeemClaimCode } from "./claim-redeem.ts";

type Sent = [string, string, string, string | null];

function recorder(reply = { author: "aa", tier: "gold", queued: true }) {
  const sent: Sent[] = [];
  const call = (s: string, c: string, a: string, h: string | null) => {
    sent.push([s, c, a, h]);
    return Promise.resolve(reply);
  };
  return { sent, call };
}

const PROFILE: ClaimRedeemTarget = {
  authorIdHex: "ab".repeat(32),
  homeNestUrl: "https://author-home.test",
};

Deno.test("a blank code is a no-op, never a nest round-trip", async () => {
  const { sent, call } = recorder();
  assertEquals(await redeemClaimCode("", "11", PROFILE, call), null);
  assertEquals(await redeemClaimCode("   \t ", "11", PROFILE, call), null);
  assertEquals(await redeemClaimCode("CODE42", null, PROFILE, call), null);
  assertEquals(sent.length, 0);
});

Deno.test("the profile door redeems the trimmed code at the viewed creator's home", async () => {
  const { sent, call } = recorder();
  const reply = await redeemClaimCode("  CODE42 ", "11", PROFILE, call);
  assertEquals(reply?.tier, "gold");
  assertEquals(sent, [["11", "CODE42", PROFILE.authorIdHex, "https://author-home.test"]]);
});

Deno.test("the settings door redeems with home null and its own id", async () => {
  const { sent, call } = recorder();
  await redeemClaimCode("CODE42", "11", { authorIdHex: "cd".repeat(32), homeNestUrl: null }, call);
  assertEquals(sent, [["11", "CODE42", "cd".repeat(32), null]]);
});

Deno.test("a refusal rejects, so the host keeps the code to correct", async () => {
  let threw = false;
  try {
    await redeemClaimCode("BAD", "11", PROFILE, () => Promise.reject(new Error("claim_not_found")));
  } catch (e) {
    threw = (e as Error).message === "claim_not_found";
  }
  assert(threw);
});

const read = (rel: string) =>
  stripComments(Deno.readTextFileSync(new URL(rel, import.meta.url)));

Deno.test("ClaimRedeem goes through the across-nests face and clears only on success", () => {
  const src = read("./ClaimRedeem.svelte");
  assert(src.includes("redeemClaimCode("), "ClaimRedeem must use the shared decision");
  assert(src.includes("paymentsClaimsRedeemAcrossNests"), "ClaimRedeem must call the across-nests face");
  assert(!/paymentsClaimsRedeem\b(?!AcrossNests)/.test(src), "the plain local redeem is retired here");
  const success = src.indexOf("if (reply === null) return;");
  const cleared = src.indexOf("claimCode = ''");
  const caught = src.indexOf("catch (e)");
  assert(success >= 0 && cleared > success && cleared < caught, "the input clears only once the nest took the code");
});

Deno.test("the profile page's redeem row names the viewed creator and the follow's home", () => {
  const src = read("../../../routes/profile/[[actorId]]/+page.svelte");
  const at = src.indexOf("<ClaimRedeem");
  assert(at >= 0, "the OTHER Tiers tab must carry the redeem row");
  const tag = src.slice(at, src.indexOf("/>", at));
  assert(tag.includes("authorIdHex={viewedActorId"), "the author slot is the viewed actor");
  assert(tag.includes("homeNestUrl={actorHomeSlot}"), "the home is the page's actor-home slot");
  // The follow reads the same slot.
  assert(src.includes("subscriptionsSubscribeAcrossNests(s, viewedActorId, FOLLOWERS_TIER, actorHomeSlot)"));
  // OTHER branch only — after the SELF branch's `{:else}` and inside the offers section.
  const offers = src.indexOf("IDS.SUBSCRIPTION_OFFERS_SECTION");
  assert(offers >= 0 && at > offers, "the row paints in the OTHER offers section, never on SELF");
  // A fresh open of another creator drops the typed code.
  const key = src.lastIndexOf("{#key viewedActorId}", at);
  assert(key > offers, "the row is keyed on the viewed actor");
  assert(src.lastIndexOf("{#if __FAUNA_PAYMENTS__}", at) > offers, "the row excises with the payments plane");
  // A queued grant reads pending, as Subscribe does.
  assert(/if \(reply\.queued\) pendingTiers = /.test(src));
});

Deno.test("the settings pair redeems with home null", () => {
  const src = read("../SubscriptionsSection.svelte");
  const at = src.indexOf("<ClaimRedeem");
  const tag = src.slice(at, src.indexOf("/>", at));
  assert(tag.includes("homeNestUrl={null}"));
  assert(tag.includes("authorIdHex={$identity?.actorId"));
});
