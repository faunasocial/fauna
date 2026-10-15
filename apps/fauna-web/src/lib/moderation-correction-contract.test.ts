// The moderation queue's server-row `train-correction-button` must run the ONE
// shared flow (`moderation.md` § State & data shape → *Training correction*;
// `mail-spam.md` § Implementation status item 4b): the sealed model write, then
// `fauna.moderation.train` ALWAYS — the nest half is the read gate and the
// report capture, so a per-app copy that returns after the sealed write (as web
// once did) leaves a correction on the sealed path unreported.
//
// `trainModerationCorrection` drives a real wasm `WasmMailSettingsMachine`,
// which this Deno unit-test environment cannot construct, so — like
// `ensure-connected-stop-contract.test.ts` — this reads the function's source
// as TEXT. The flow's own two halves are pinned in Rust
// (`libs/fauna-client-mail-settings/tests/spam_model_write.rs`).

import { stripComments } from "./source-contract.ts";

const CONVERSATIONS = new URL("./conversations.ts", import.meta.url);

/** `trainModerationCorrection`'s body, from its declaration to the next
 *  top-level declaration. */
function correctionBody(): string {
  const src = stripComments(Deno.readTextFileSync(CONVERSATIONS));
  const start = src.indexOf("export async function trainModerationCorrection(");
  if (start < 0) {
    throw new Error(
      "conversations.ts no longer declares `trainModerationCorrection` — this " +
        "contract's anchor is gone; re-point it at whatever now owns the " +
        "moderation-queue correction.",
    );
  }
  const rest = src.slice(start + 1);
  const end = rest.search(/\n(?:export )?(?:async )?function /);
  return end < 0 ? rest : rest.slice(0, end);
}

Deno.test("a server-row correction is one call to the shared flow", () => {
  const body = correctionBody();
  if (!body.includes(".trainModerationCorrection(contentId, false)")) {
    throw new Error(
      "trainModerationCorrection no longer calls the shared " +
        "`trainModerationCorrection(contentId, false)` for a server row — the " +
        "sealed write and `fauna.moderation.train` must both run through it.",
    );
  }
});

Deno.test("no per-app copy of the sealed-write early return survives", () => {
  const body = correctionBody();
  for (const forbidden of ["outcome.sealed", "moderationTrain(", "sealedSpamWriteAvailable"]) {
    if (body.includes(forbidden)) {
      throw new Error(
        `trainModerationCorrection mentions \`${forbidden}\` — a per-app copy ` +
          "of the flow that can skip `fauna.moderation.train` after a sealed write.",
      );
    }
  }
});
