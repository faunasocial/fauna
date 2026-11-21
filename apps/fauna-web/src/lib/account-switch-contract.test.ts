// The one-switch contract (`docs/goal/behavior/onboarding.md` § Wizard exit
// handling → *The finished wizard holds*; `long-term-store.md` § Multi-account
// evolution → *Per-account re-auth*), asserted over the source (testing.md
// convention 17): both web switchers — Settings' and the one `launch_retry`
// carries — run the SAME switch (`#lib/account-switch`) and paint the SAME
// in-app re-auth prompt, so the gate, the leave gestures and the relaunch
// cannot drift between them.
//
//  1. Neither page calls the registry's activation itself: `accountsSwitch(` /
//     `accountsSwitchConfirmed(` live in `#lib/account-switch` (the onboarding
//     page keeps its own append-finish `accountsSwitch`, outside the switcher).
//  2. Each page asks `switchReauthTarget(` before switching, switches through
//     `performAccountSwitch(`, words a refusal with `switchRefusalText(`, and
//     paints `<AccountReauthPrompt`.
//  3. `confirmed = true` is passed from exactly one site per page — the
//     prompt's confirm — the gate's audit surface.
//  4. The launch_retry switcher shows only past one account.
//
// Crude like its siblings: reads source as TEXT with comments stripped.

import { assert, assertEquals } from "jsr:@std/assert";
import { stripComments } from "./source-contract.ts";

const read = (rel: string) => Deno.readTextFile(new URL(rel, import.meta.url)).then(stripComments);

const SETTINGS = "../routes/settings/[[subpage]]/+page.svelte";
const ONBOARDING = "../routes/onboarding/+page.svelte";

Deno.test("both switchers run the shared switch and the shared re-auth prompt", async () => {
  for (const page of [SETTINGS, ONBOARDING]) {
    const code = await read(page);
    for (const call of ["switchReauthTarget(", "performAccountSwitch(", "switchRefusalText(", "<AccountReauthPrompt"]) {
      assert(code.includes(call), `${page} no longer uses ${call} — the switch must stay shared`);
    }
    assert(!code.includes("accountsSwitchConfirmed("), `${page} activates a flagged account itself`);
  }
  const settings = await read(SETTINGS);
  assert(!/\baccountsSwitch\(/.test(settings), "Settings activates an account outside the shared switch");
});

Deno.test("confirmed=true is passed from one site per switcher", async () => {
  const settings = await read(SETTINGS);
  assertEquals(settings.match(/performSwitch\([^)]*,\s*true\)/g)?.length, 1);
  const onboarding = await read(ONBOARDING);
  assertEquals(onboarding.match(/runLaunchSwitch\([^)]*,\s*true\)/g)?.length, 1);
});

Deno.test("launch_retry paints the switcher only past one account", async () => {
  const onboarding = await read(ONBOARDING);
  assert(
    onboarding.includes("{#if launchSwitcherRows.length > 1}"),
    "launch_retry's switcher must stay hidden with one account or none",
  );
});
