// Every page-machine wasm chunk rides the SPA singleton's ONE socket through
// the shared rpc port — asserted as a general invariant over the source rather
// than as one more hand-picked outcome (testing.md convention 17). The
// transport-side sibling of `client-retire-drop-contract.test.ts`, aimed at the
// shape that produced the bug this contract closes.
//
// ── The class ─────────────────────────────────────────────────────────────
//
// `transport.md` § Goal: the authenticated UI surface is a single WebSocket per
// actor. Until 2026-09-25 web broke it six times over: every page-machine chunk
// (folders ×2, media, backups, labeler catalog, atproto settings) took the
// `(nestUrl, actorIdHex, tokenProvider)` triple and dialled its own
// `WsRpcClient`, each with its own jittered reconnect loop (ceiling 60 s). After
// a nest restart the app's `connection` observable read online — the core
// singleton was back — while a page's machine was still asleep in ITS backoff,
// and a Folders gesture answered `not connected`.
//
// The fix is one seam: `rpc.ts::sharedRpcPort(secretHex)` implements the typed
// `SharedRpcPort` the chunks declare (`fauna_rpc_wasm::shared_port`, emitted
// into each chunk's `.d.ts`), and every chunk constructor takes exactly that
// port. Pure data crosses the chunk boundary — kind strings, canonical-CBOR
// bytes, the bearer — never a wasm-bindgen object (`apps/web.md` § WASM
// Integration's discipline).
//
// ── What this pins ────────────────────────────────────────────────────────
//
// The seam is only as good as its adoption, and adoption is the thing a later
// chunk (or a later "quick" page) forgets: the triple is still what
// `WsRpcClient.connect` takes, so the old shape stays *available* to anyone
// who copies an old loader. So: no `lib/wasm-*.ts` loader may accept a
// `tokenProvider` (the tell-tale of a chunk-private dial), and every loader
// that builds a machine over the nest must type its transport as
// `SharedRpcPort`. And the port's one implementation must resolve the CURRENT
// singleton per call (`getClient(secretHex)` inside `request`), never capture
// the client it was built over — a machine that outlives an identity/nest swap
// must follow the socket, not hold a retired one.
//
// Deliberately crude, like its siblings: it reads source as TEXT.

import { stripComments } from "./source-contract.ts";

const HERE = new URL("./", import.meta.url);

/** The loaders of the chunks that talk to the nest through a page machine. */
const NEST_FACING_LOADERS = [
  "wasm-folders.ts",
  "wasm-media.ts",
  "wasm-backups.ts",
  "wasm-labeler-catalog.ts",
  "wasm-connected-apps.ts",
  "wasm-atproto-settings.ts",
];

function source(name: string): string {
  return stripComments(Deno.readTextFileSync(new URL(name, HERE)));
}

Deno.test("no wasm loader takes a tokenProvider — chunks never dial their own socket", () => {
  for (const entry of Deno.readDirSync(HERE)) {
    if (!entry.isFile || !/^wasm-.*\.ts$/.test(entry.name) || entry.name.endsWith(".test.ts")) continue;
    const src = source(entry.name);
    if (/\btokenProvider\b/.test(src)) {
      throw new Error(
        `${entry.name} takes a tokenProvider — the tell-tale of a chunk-private ` +
          `WsRpcClient dial. A page-machine chunk builds over the SPA singleton's ` +
          `socket: take a SharedRpcPort from $lib/rpc's sharedRpcPort(secretHex) ` +
          `instead (transport.md § Goal: one WebSocket per actor).`,
      );
    }
  }
});

Deno.test("every nest-facing loader types its transport as SharedRpcPort", () => {
  for (const name of NEST_FACING_LOADERS) {
    const src = source(name);
    if (!/\bport:\s*SharedRpcPort\b/.test(src)) {
      throw new Error(
        `${name} builds a machine over the nest but no create function takes ` +
          `\`port: SharedRpcPort\` — the chunk's own .d.ts declares the interface; ` +
          `import it from the chunk's static module and hand it to the constructor.`,
      );
    }
  }
});

Deno.test("sharedRpcPort resolves the live singleton on every request", () => {
  const src = source("rpc.ts");
  const start = src.indexOf("export async function sharedRpcPort(");
  if (start < 0) {
    throw new Error(
      "rpc.ts no longer declares `export async function sharedRpcPort(` — the " +
        "one implementation of SharedRpcPort moved; re-point this contract.",
    );
  }
  const rest = src.slice(start + 1);
  const end = rest.search(/\n(?:export )?(?:async )?function /);
  const body = end < 0 ? rest : rest.slice(0, end);
  if (!/request:\s*async\s*\([^)]*\)\s*=>\s*\{[^}]*await getClient\(secretHex\)/.test(body)) {
    throw new Error(
      "sharedRpcPort's `request` must `await getClient(secretHex)` on EACH call: " +
        "getClient re-keys on an identity or nest change and retires the old " +
        "client (close() is one-way), and a chunk machine that outlives that " +
        "swap — the devices session memo, the atproto singleton — must follow the " +
        "socket rather than hold a retired one.",
    );
  }
  if (!body.includes(".requestRaw(")) {
    throw new Error(
      "sharedRpcPort's `request` must run through the core chunk's " +
        "`WsRpcClient.requestRaw` — the owner's half of the port, which takes the " +
        "same reconnect-wait and per-kind deadline every core request takes.",
    );
  }
});
