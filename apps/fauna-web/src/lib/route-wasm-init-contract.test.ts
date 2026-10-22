// No route reads a wasm chunk synchronously while its component is being
// constructed (web.md § the WASM chunk inventory: every sync wasm read sits
// behind its chunk's `ensure*Wasm()`), asserted over the source.
//
// The root layout renders its child route before the core chunk is up, and a
// route's top-level `<script>` runs before its `onMount`, where routes await
// `ensureWasm()`. A synchronous wasm read called from a top-level initializer
// therefore throws `WASM not initialized — call ensureWasm() first` on every
// cold load of that route, and the throw blanks the page. The settings page
// did exactly that with `let pushSubscribed = $state(pushOptedIn())`: every
// settings sub-page cold load died, which failed the box-recovery journey's
// web leg at its claim and parked a restored tab whose pin had gone (its
// `onMount` never reached `identity.init()`). The
// same page had already lost a top-level `inboxModeValues()` read to the same
// race, and the search page carried a third (`searchTypeFilterOptions()`).
//
// The rule: a 2-space-indented `let`/`const` initializer in a route's script
// may not call a synchronous wasm read, bare or inside `$state(...)`, whichever
// `#lib` module it is imported through. `$derived(...)` is exempt — it is lazy,
// evaluated at its first read, and the routes gate their template on a
// post-`ensureWasm()` loading flag. Seed a plain default and assign the real
// read after `ensureWasm()`.

import { stripComments } from "./source-contract.ts";

const LIB = new URL("./", import.meta.url);
const ROUTES = new URL("../routes/", import.meta.url);

/** `{ a, b as c, type T }` → [[local, original], …], types dropped. */
function names(list: string): Array<[string, string]> {
  const out: Array<[string, string]> = [];
  for (const raw of list.split(",")) {
    const part = raw.trim();
    if (!part || part.startsWith("type ")) continue;
    const [orig, local] = part.split(/\s+as\s+/);
    out.push([(local ?? orig).trim(), orig.trim()]);
  }
  return out;
}

/** Local name → { module, original name } for the named imports whose
 *  specifier matches `specifier` (its one group is the module name). */
function importsFrom(code: string, specifier: string): Map<string, { mod: string; orig: string }> {
  const out = new Map<string, { mod: string; orig: string }>();
  const re = new RegExp(String.raw`import\s*\{([^}]*)\}\s*from\s*` + specifier, "g");
  for (const m of code.matchAll(re)) {
    for (const [local, orig] of names(m[1])) out.set(local, { mod: m[2], orig });
  }
  return out;
}

/** Non-`async` `export function`s in one module whose body reaches its chunk
 *  accessor `wasm()` — every `wasm*.ts` loader names its accessor that. */
function directSyncReads(code: string): Set<string> {
  const decls = [...code.matchAll(/\n(export )?(async )?function ([A-Za-z0-9_]+)\s*[(<]/g)]
    .map((m) => ({ at: m.index!, exported: !!m[1], isAsync: !!m[2], name: m[3] }));
  const out = new Set<string>();
  decls.forEach((d, i) => {
    if (!d.exported || d.isAsync) return;
    const body = code.slice(d.at, i + 1 < decls.length ? decls[i + 1].at : code.length);
    if (/\bwasm\(\)/.test(body)) out.add(d.name);
  });
  return out;
}

/** `#lib/<module>` → its sync wasm reads, re-exports followed: `push.ts`
 *  re-exports `wasm.ts`'s `pushOptedIn`, and the settings page imported it
 *  from `#lib/push` — a scan keyed on `#lib/wasm` alone passed that page. */
async function syncReadsByModule(): Promise<Map<string, Set<string>>> {
  const code = new Map<string, string>();
  for await (const e of Deno.readDir(LIB)) {
    if (!e.isFile || !e.name.endsWith(".ts") || e.name.endsWith(".test.ts")) continue;
    code.set(e.name.slice(0, -3), stripComments(await Deno.readTextFile(new URL(e.name, LIB))));
  }
  const reads = new Map([...code].map(([mod, src]) => [mod, directSyncReads(src)]));
  let changed = true;
  while (changed) {
    changed = false;
    for (const [mod, src] of code) {
      const imported = importsFrom(src, String.raw`'\./([A-Za-z0-9_-]+)'`);
      for (const m of src.matchAll(/export\s*\{([^}]*)\}(?:\s*from\s*'\.\/([A-Za-z0-9_-]+)')?/g)) {
        for (const [local, orig] of names(m[1])) {
          const from = m[2] ? { mod: m[2], orig } : imported.get(local);
          const own = reads.get(mod)!;
          if (from && reads.get(from.mod)?.has(from.orig) && !own.has(local)) {
            own.add(local);
            changed = true;
          }
        }
      }
    }
  }
  return reads;
}

async function* svelteFiles(dir: URL): AsyncGenerator<URL> {
  for await (const e of Deno.readDir(dir)) {
    const u = new URL(e.name + (e.isDirectory ? "/" : ""), dir);
    if (e.isDirectory) yield* svelteFiles(u);
    else if (e.name.endsWith(".svelte")) yield u;
  }
}

function topLevelViolations(src: string, reads: Map<string, Set<string>>): string[] {
  const script = src.match(/<script[^>]*>([\s\S]*?)<\/script>/)?.[1] ?? "";
  const code = stripComments(script);
  const imports = importsFrom(code, String.raw`'#lib/([A-Za-z0-9_-]+)'`);
  const bad: string[] = [];
  for (const m of code.matchAll(/\n {2}(?:let|const) [^=\n]+=\s*([^\n;]*)/g)) {
    const init = m[1];
    if (/^\$derived/.test(init)) continue;
    for (const call of init.matchAll(/\b([A-Za-z0-9_]+)\s*\(/g)) {
      const from = imports.get(call[1]);
      if (from && reads.get(from.mod)?.has(from.orig)) bad.push(m[0].trim());
    }
  }
  return bad;
}

Deno.test("the sync-read scan finds the reads it must ban", async () => {
  const reads = await syncReadsByModule();
  // A scan that silently matched nothing would pass every route.
  const want: Array<[string, string]> = [
    ["wasm", "pushOptedIn"],
    ["wasm", "searchTypeFilterOptions"],
    ["push", "pushOptedIn"],
  ];
  for (const [mod, name] of want) {
    if (!reads.get(mod)?.has(name)) throw new Error(`scan missed #lib/${mod}::${name}`);
  }
  if (reads.get("wasm")?.has("ensureWasm")) throw new Error("ensureWasm is async, not a sync read");
});

Deno.test("the scan reds the shape that blanked the settings page", () => {
  const src = `<script lang="ts">
  import { pushOptedIn } from '#lib/push';
  import { reportLedgerWords } from '#lib/wasm';
  let pushSubscribed = $state(pushOptedIn());
  const words = $derived(reportLedgerWords());
</script>`;
  const reads = new Map([["push", new Set(["pushOptedIn"])], ["wasm", new Set(["reportLedgerWords"])]]);
  const bad = topLevelViolations(src, reads);
  if (bad.length !== 1 || !bad[0].includes("pushOptedIn")) {
    throw new Error(`want exactly the $state(pushOptedIn()) line, got ${JSON.stringify(bad)}`);
  }
});

Deno.test("no route reads a wasm chunk at component construction", async () => {
  const reads = await syncReadsByModule();
  const failures: string[] = [];
  for await (const file of svelteFiles(ROUTES)) {
    for (const line of topLevelViolations(await Deno.readTextFile(file), reads)) {
      failures.push(`${file.pathname.split("/src/routes/")[1]}: ${line}`);
    }
  }
  if (failures.length) {
    throw new Error(
      "sync wasm read in a route's top-level script — it runs before onMount's " +
        "ensureWasm(); seed a default and read after ensureWasm():\n  " +
        failures.join("\n  "),
    );
  }
});
