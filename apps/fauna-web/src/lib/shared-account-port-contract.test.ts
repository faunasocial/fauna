// Every wasm chunk that needs the store reaches this tab's account runtime
// through the ONE account port — asserted as a general invariant over the
// source rather than as one more hand-picked outcome (testing.md convention
// 17). The store-side sibling of `shared-rpc-port-contract.test.ts`.
//
// ── The shape ─────────────────────────────────────────────────────────────
//
// `account-client-lifecycle.md` § The client-side lifecycle → *The account
// port*: the runtime's `AccountStoreHandle` lives in the core chunk's memory,
// so a page machine in another chunk (the Devices page's in the folders chunk
// first) reaches it through the typed `SharedAccountPort { call(door,
// payload) }` the chunks declare (`fauna_account_port`, emitted into each
// chunk's `.d.ts`). `account-runtime.ts::sharedAccountPort(secretHex)` is its
// one implementation, forwarding to the core chunk's `accountPortCall` with
// the account's actor id bound at mint (decision (e)).
//
// ── What this pins ────────────────────────────────────────────────────────
//
// 1. One implementation: only `account-runtime.ts` mints a port, and only
//    through `accountPortCall` — a second, hand-rolled port could forget the
//    actor id and let a machine that outlived an account switch write into
//    the next account's store.
// 2. Every chunk machine wired with a port is wired with THAT one: each
//    `.setAccountPort(` call passes `sharedAccountPort(`.
// 3. Every loader that takes a port types it `SharedAccountPort`, never `any`.
//
// Deliberately crude, like its siblings: it reads source as TEXT.

import { stripComments } from "./source-contract.ts";

const HERE = new URL("./", import.meta.url);
const SRC = new URL("../", HERE);

function source(url: URL): string {
  return stripComments(Deno.readTextFileSync(url));
}

/** Every `.ts` / `.svelte` file under `src/`, tests excluded. */
function* sources(dir: URL = SRC): Generator<[string, string]> {
  for (const entry of Deno.readDirSync(dir)) {
    const url = new URL(entry.name + (entry.isDirectory ? "/" : ""), dir);
    if (entry.isDirectory) {
      yield* sources(url);
    } else if (/\.(ts|svelte)$/.test(entry.name) && !entry.name.endsWith(".test.ts")) {
      yield [url.pathname.slice(SRC.pathname.length), source(url)];
    }
  }
}

Deno.test("sharedAccountPort is the one implementation, over accountPortCall", () => {
  const src = source(new URL("account-runtime.ts", HERE));
  const start = src.indexOf("export function sharedAccountPort(");
  if (start < 0) {
    throw new Error(
      "account-runtime.ts no longer declares `export function sharedAccountPort(` — " +
        "the one implementation of SharedAccountPort moved; re-point this contract.",
    );
  }
  const rest = src.slice(start + 1);
  const end = rest.search(/\n(?:export )?(?:async )?function /);
  const body = end < 0 ? rest : rest.slice(0, end);
  if (!/actorIdFromSecret\(secretHex\)/.test(body) || !/accountPortCall\(actorIdHex,/.test(body)) {
    throw new Error(
      "sharedAccountPort must bind the account's actor id at mint " +
        "(`actorIdFromSecret(secretHex)`) and forward every call to the core chunk's " +
        "`accountPortCall(actorIdHex, …)` — the core side refuses a call for an " +
        "account its running runtime does not serve (decision (e)).",
    );
  }
  for (const [name, text] of sources()) {
    if (name === "lib/wasm.ts" || name === "lib/account-runtime.ts") continue;
    if (/\baccountPortCall\b/.test(text)) {
      throw new Error(
        `${name} calls accountPortCall directly — a second account port. Take ` +
          `sharedAccountPort(secretHex) from $lib/account-runtime instead.`,
      );
    }
  }
});

/**
 * The chunk methods that take the account port as an argument rather than
 * through a wired machine's `setAccountPort`: the custody facet's load (the
 * ceremony records and the grant log) and revoke (the grant log), and every
 * followed-folders surface (the account's `fauna.state.follows` rows) — the
 * follow, unfollow and list faces and the Devices and Media pages'
 * followed-folders sources.
 */
const PORT_TAKING = [
  "custodyFacetLoad",
  "custodyRevoke",
  "followPublicFolder",
  "unfollowPublicFolder",
  "followedFolders",
  "setFollowedFoldersSource",
  "setFollowedMediaSource",
];

/** The argument text of every `.method(…)` call in `text`, parentheses balanced. */
function* callArguments(text: string, method: string): Generator<string> {
  const open = `.${method}(`;
  for (let at = text.indexOf(open); at >= 0; at = text.indexOf(open, at + 1)) {
    let depth = 1;
    let i = at + open.length;
    for (; i < text.length && depth > 0; i++) {
      if (text[i] === "(") depth++;
      else if (text[i] === ")") depth--;
    }
    yield text.slice(at + open.length, i - 1);
  }
}

Deno.test("a port-taking call is handed sharedAccountPort", () => {
  for (const method of PORT_TAKING) {
    let calls = 0;
    for (const [name, text] of sources()) {
      for (const args of callArguments(text, method)) {
        calls++;
        if (!/sharedAccountPort\(/.test(args)) {
          throw new Error(
            `${name} calls ${method} without sharedAccountPort(secretHex) — ` +
              "the account's records cross the one account port.",
          );
        }
      }
    }
    if (calls === 0) {
      throw new Error(
        `no \`.${method}(\` call left in src/ — re-point this contract if the ` +
          "call moved.",
      );
    }
  }
});

Deno.test("no store crossing besides the account port remains", () => {
  // The interim ledger-only port (`SuccessionLedgerPort` over the core chunk's
  // `accountLedger*` exports) was folded into the account port's
  // succession-ledger seam; a crossing of its own would be a second door into
  // the runtime that binds no account at mint.
  for (const [name, text] of sources()) {
    const m = /\b(SuccessionLedgerPort|accountLedger\w*)\b/.exec(text);
    if (m) {
      throw new Error(
        `${name} names \`${m[1]}\` — the succession ledger crosses the account ` +
          "port (`sharedAccountPort(secretHex)`), not a port of its own.",
      );
    }
  }
});

Deno.test("every chunk machine is wired with sharedAccountPort", () => {
  let wired = 0;
  for (const [name, text] of sources()) {
    for (const m of text.matchAll(/\.setAccountPort\(([^)]*)/g)) {
      wired++;
      if (!/^\s*sharedAccountPort\(/.test(m[1])) {
        throw new Error(
          `${name} wires setAccountPort(${m[1]}…) — pass sharedAccountPort(secretHex), ` +
            `the one port implementation.`,
        );
      }
    }
  }
  if (wired === 0) {
    throw new Error(
      "no `.setAccountPort(` call left in src/ — the Devices page's fleet door is " +
        "unwired, and a web removal deletes the nest row alone; re-point this contract " +
        "if the wiring moved.",
    );
  }
});

Deno.test("every loader that takes an account port types it SharedAccountPort", () => {
  for (const entry of Deno.readDirSync(HERE)) {
    if (!entry.isFile || !/^wasm-.*\.ts$/.test(entry.name) || entry.name.endsWith(".test.ts")) continue;
    const text = source(new URL(entry.name, HERE));
    for (const m of text.matchAll(/\baccountPort\w*\s*:\s*([A-Za-z_]+)/g)) {
      if (m[1] !== "SharedAccountPort") {
        throw new Error(
          `${entry.name} takes an account port typed \`${m[1]}\` — type it ` +
            "`SharedAccountPort` (the chunk's own .d.ts declares it).",
        );
      }
    }
  }
});
