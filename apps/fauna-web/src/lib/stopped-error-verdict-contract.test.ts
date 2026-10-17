// `stoppedError` (`rpc.ts`) must type every session-ending verdict the wasm
// client's `sessionEndingVerdict()` answers, and route each through the
// launch-route re-entry — keyed on the typed verdict, never on the stop's
// localized text. The app-outdated arm is the other direction of
// `nest_outdated` (a 4426 close: this app is too old for its nest), kept apart
// from it at this seam as at every other (`version-compatibility.md`
// § Dimension 4, *The other direction*; `transport-connection.md`
// § Connection lifecycle, the 4426 paragraph).
//
// `stoppedError` takes a real `WsRpcClient` (a wasm binding) and its
// escalation navigates the document, neither of which this Deno environment
// can drive — so, like `ensure-connected-stop-contract.test.ts` beside it,
// this reads the function's source as TEXT.

import { stripComments } from "./source-contract.ts";

const RPC = new URL("./rpc.ts", import.meta.url);
const ESCALATION = new URL("./post-auth-escalation.ts", import.meta.url);

/** A top-level function's body, from its declaration to the next one. */
function functionBody(src: string, decl: string): string {
  const start = src.indexOf(decl);
  if (start < 0) {
    throw new Error(`the source no longer declares \`${decl}\` — this contract's anchor is gone`);
  }
  const rest = src.slice(start + 1);
  const end = rest.search(/\n(?:export )?(?:async )?function /);
  return end < 0 ? rest : rest.slice(0, end);
}

const VERDICTS: Array<[string, string]> = [
  ["sign_in_refused", "SignInRefusedError"],
  ["nest_outdated", "NestOutdatedError"],
  ["app_outdated", "AppOutdatedError"],
];

for (const [verdict, errorClass] of VERDICTS) {
  Deno.test(`stoppedError types the '${verdict}' verdict as ${errorClass}`, () => {
    const body = functionBody(stripComments(Deno.readTextFileSync(RPC)), "function stoppedError(");
    const arm = new RegExp(`case\\s*'${verdict}'\\s*:\\s*verdict\\s*=\\s*new\\s+${errorClass}\\(`);
    if (!arm.test(body)) {
      throw new Error(
        `stoppedError has no \`case '${verdict}': verdict = new ${errorClass}(\` arm — a ` +
          `reconnect loop stopped on that verdict would fail every call with an ` +
          `untyped Error and never reach the launch surface.`,
      );
    }
  });
}

Deno.test("stoppedError escalates the typed verdict", () => {
  const body = functionBody(stripComments(Deno.readTextFileSync(RPC)), "function stoppedError(");
  if (!body.includes("escalateIfTerminalAuthVerdict(verdict)")) {
    throw new Error("stoppedError no longer escalates the verdict it types");
  }
});

Deno.test("the escalation routes AppOutdatedError to the launch surface, typed and classified", () => {
  const body = functionBody(
    stripComments(Deno.readTextFileSync(ESCALATION)),
    "export function escalateIfTerminalAuthVerdict(",
  );
  for (const form of ["e instanceof AppOutdatedError", "verdict instanceof AppOutdatedError"]) {
    if (!body.includes(form)) {
      throw new Error(
        `escalateIfTerminalAuthVerdict lacks \`${form}\` — the app-outdated verdict ` +
          `must escalate whether the caller holds the typed error or the raw prefix.`,
      );
    }
  }
  if (!/AppOutdatedError\)\s*return\s+escalateAppOutdated\(\)/.test(body)) {
    throw new Error("an AppOutdatedError must route to escalateAppOutdated, not the nest-outdated escalation");
  }
});
