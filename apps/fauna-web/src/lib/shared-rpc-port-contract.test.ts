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
          `socket: take a SharedRpcPort from #lib/rpc's sharedRpcPort(secretHex) ` +
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
  const req = body.slice(body.indexOf("request:"));
  const refuse = req.search(/throw new Error\(\s*'shared rpc port refused/);
  const dial = req.indexOf("await getClient(secretHex)");
  if (dial < 0) {
    throw new Error(
      "sharedRpcPort's `request` must `await getClient(secretHex)` on EACH call: " +
        "getClient re-keys on a nest change and retires the old client (close() " +
        "is one-way), and a chunk machine that outlives that swap — the devices " +
        "session memo, the atproto singleton — must follow the socket rather " +
        "than hold a retired one.",
    );
  }
  // A port is minted for one actor: once the identity moved on in
  // place, getClient(secretHex) would re-key the singleton BACK to the retired
  // actor, retiring the current identity's client. So the refusal must come
  // first, keyed on the port's actor vs the singleton's and the current identity.
  if (refuse < 0 || refuse > dial) {
    throw new Error(
      "sharedRpcPort's `request` must refuse (throw 'shared rpc port refused…') " +
        "BEFORE `await getClient(secretHex)` when the port's actor is no longer " +
        "the current one — otherwise a stale port retires the current identity's " +
        "client and dials the old one.",
    );
  }
  // The clauses themselves are pinned behaviourally (`portIsStale`, in
  // shared-port-and-poll-behaviour.test.ts); here only that the refusal is that
  // predicate, fed the port's actor, the singleton's and the CURRENT identity's.
  if (!/portIsStale\(\s*mine,\s*clientActorId,\s*currentActorId\(\)\s*\)/.test(req)) {
    throw new Error(
      "the stale-port refusal must be `portIsStale(mine, clientActorId, " +
        "currentActorId())` — fed BOTH the singleton's actor and the current " +
        "identity's, the identity clause being the only guard in the " +
        "post-`retireRpcClient` window.",
    );
  }
  // The call text alone left the operand's SOURCE free: a mutant that
  // kept the call but fed it from anything but the identity store stayed green.
  // `currentActorId` is that source, so pin its body to `get(identity)`.
  if (
    !/export function currentActorId\(\)[^{]*\{\s*const current = get\(identity\);\s*return current \? actorIdFromSecret\(current\.secretHex\) : null;\s*\}/
      .test(src)
  ) {
    throw new Error(
      "rpc.ts's `currentActorId()` must read `get(identity)` and map it to " +
        "`actorIdFromSecret(current.secretHex)` or null — it is the stale-port " +
        "refusal's identity operand, and the only guard in the " +
        "post-`retireRpcClient` window; a neutered source would let a stale " +
        "port re-key the singleton back to the retired actor.",
    );
  }
  // Both pins above are source TEXT, so a local rebinding inside `request`
  // (`const currentActorId = () => null;`) leaves them green while the call
  // site's identity operand goes vacuous. The exported function must be the
  // ONLY binding of the name in rpc.ts: no other declaration, assignment,
  // parameter or destructuring (object `{ currentActorId }` and array
  // `[currentActorId]` forms alike).
  const bindings = [
    ...src.matchAll(/\b(?:const|let|var|function|class)\s+currentActorId\b/g),
    ...src.matchAll(/\bcurrentActorId\s*(?:=(?!=)|[,)}\]:])/g),
    ...src.matchAll(/\bcurrentActorId\s*\)\s*(?:=>|\{)/g),
  ];
  if (bindings.length !== 1) {
    throw new Error(
      "rpc.ts must bind `currentActorId` exactly once — the exported function. " +
        `Found ${bindings.length} binding-shaped occurrences: a local shadow ` +
        "(const/let/function/parameter/destructuring) inside `request` would " +
        "leave the call text and the function body pins green while the stale-port " +
        "refusal's identity operand is neutered.",
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

// The 15 s page-machine polls write the current actor's state, so
// the canonical drop must retire them. Both build the poll with
// `createActorScopedPoll` (behaviour pinned in
// shared-port-and-poll-behaviour.test.ts: it registers with the actor-scoped
// reset, refuses to start after a drop, stops on one); here the adoption — no
// component hand-rolls a `setInterval` again, and each starts the poll under a
// `sameActorSince()` captured for its mount and disposes it on destroy.
Deno.test("the Folders and Bluesky settings polls are the actor-scoped poll", () => {
  for (const name of ["components/FoldersSection.svelte", "components/AtprotoSettingsSection.svelte"]) {
    const src = Deno.readTextFileSync(new URL(name, HERE));
    if (
      !/createActorScopedPoll\(/.test(src) ||
      !/const sameActor = sameActorSince\(\)/.test(src) ||
      !/poll\.start\(sameActor,/.test(src) ||
      !/poll\.dispose\(\)/.test(src) ||
      /\bsetInterval\(/.test(stripComments(src))
    ) {
      throw new Error(
        `${name} must run its poll through createActorScopedPoll — ` +
          `\`poll.start(sameActor, …)\` with \`sameActor = sameActorSince()\` captured ` +
          `before the mount's first await, \`poll.dispose()\` on destroy, and no ` +
          `hand-rolled setInterval: a poll that outlives an in-place identity ` +
          `change pings the old actor's machine (account-scoping.md § The ` +
          `scoping taxonomy).`,
      );
    }
  }
});
