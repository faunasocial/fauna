// The Folders page's default-conflict-policy load must say WHY it did not load
// when the first-listing gate refused it (`common.needs_nest` /
// `common.needs_other_device`) — `account-client-lifecycle.md` § The
// client-side lifecycle → *The first listing*, clauses (4) and (5). The load
// degrades every other failure quietly to "auto"; only the refusal is shown on
// the page's `error-message` (the section's bound `error`).
//
// `FoldersSection.svelte` needs the wasm bindings, which this Deno unit
// environment cannot construct, so — like `ensure-connected-stop-contract.test.ts`
// — this reads the function's source as TEXT: aimed at the recurring shape
// (the refusal branch getting deleted), not a parse.

import { stripComments } from "./source-contract.ts";

const SECTION = new URL("./components/FoldersSection.svelte", import.meta.url);

function loadBody(): string {
  const src = stripComments(Deno.readTextFileSync(SECTION));
  const start = src.indexOf("async function loadDefaultConflictPolicy(");
  if (start < 0) {
    throw new Error(
      "FoldersSection.svelte no longer declares `loadDefaultConflictPolicy` — " +
        "re-point this contract at whatever now loads the default policy.",
    );
  }
  const rest = src.slice(start + 1);
  const end = rest.search(/\n  \/\*\*|\n  async function |\n  let /);
  return end < 0 ? rest : rest.slice(0, end);
}

Deno.test("default-conflict-policy load shows the first-listing refusal on error-message", () => {
  const body = loadBody();
  if (!/refusedAsNotReady\(\s*reason\s*\)/.test(body)) {
    throw new Error("the load's catch no longer asks the shared `refusedAsNotReady` predicate");
  }
  if (!/error\s*=\s*reason/.test(body)) {
    throw new Error("a recognised refusal must be written to the bound `error` (error-message)");
  }
});

Deno.test("default-conflict-policy load still degrades other failures to auto", () => {
  const body = loadBody();
  if (!/defaultConflictPolicy\s*=\s*'auto'/.test(body.slice(body.indexOf("catch")))) {
    throw new Error("the catch must still paint 'auto' (no empty state, no blocked section)");
  }
});
