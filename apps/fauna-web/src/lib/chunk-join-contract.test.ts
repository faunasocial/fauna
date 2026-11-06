// Every page-level wasm chunk joins the page through `joinChunk` — asserted
// over the source rather than per chunk (testing.md convention 17).
//
// Each chunk is its own Rust runtime with its own `tracing` dispatcher, and
// only the core chunk (`fauna-wasm`) holds the log ring. A chunk whose loader
// skips `joinChunk` sends a shared crate's log line nowhere on web
// (`observability.md` § Persistence & privacy, the Web bullet), and a chunk
// whose crate exports no `setLogSink` gives `joinChunk` nothing to call. Both
// halves are pinned here, so a tenth chunk cannot quietly ship without either.
//
// Two chunks have no `joinChunk` call, each for a stated reason:
// - `fauna-wasm-share` runs only on the share viewer page, which loads no core
//   chunk and so has no ring to reach;
// - `fauna-wasm-content-index` is on hold, unbuilt and with no caller
//   (`wasm-content-index.ts`).
// Both crates still export `setLogSink`, so reviving either is one call.
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`

const LIB = new URL("./", import.meta.url);
const LIBS = new URL("../../../../libs/", import.meta.url);

/** Chunks that are not page-level: the core chunk holds the ring itself, the
 *  panic-hook crate is the shared library, and `fauna-wasm-onnx` is a vendored
 *  package directory, not a crate of ours. */
const NOT_A_PAGE_CHUNK = new Set(["fauna-wasm", "fauna-wasm-panic-hook", "fauna-wasm-onnx"]);
/** Loaders with no `joinChunk` call — see the header for why. */
const LOADER_EXEMPT = new Set(["wasm-content-index.ts"]);

function chunkCrates(): string[] {
  const out: string[] = [];
  for (const e of Deno.readDirSync(LIBS)) {
    if (e.isDirectory && e.name.startsWith("fauna-wasm") && !NOT_A_PAGE_CHUNK.has(e.name)) {
      out.push(e.name);
    }
  }
  return out.sort();
}

Deno.test("every page-level wasm chunk crate exports setLogSink", () => {
  const crates = chunkCrates();
  if (crates.length < 10) throw new Error(`found only ${crates.length} chunk crates: ${crates}`);
  const missing = crates.filter((c) => {
    const src = Deno.readTextFileSync(new URL(`${c}/src/lib.rs`, LIBS));
    return !/#\[wasm_bindgen\(js_name = setLogSink\)\]\s*pub fn set_log_sink\(sink: js_sys::Function\)\s*\{\s*fauna_wasm_panic_hook::forward_tracing\(sink\);/.test(src);
  });
  if (missing.length) throw new Error(`chunks with no setLogSink forwarding: ${missing.join(", ")}`);
});

Deno.test("every page-level chunk loader calls joinChunk once its module is up", () => {
  const loaders: string[] = [];
  for (const e of Deno.readDirSync(LIB)) {
    if (/^wasm-.+\.ts$/.test(e.name) && !e.name.includes(".test.") && e.name !== "wasm-loader-guard.ts") {
      const src = Deno.readTextFileSync(new URL(e.name, LIB));
      if (src.includes("mod.default(")) loaders.push(e.name);
    }
  }
  if (loaders.length < 8) throw new Error(`found only ${loaders.length} loaders: ${loaders}`);
  const missing = loaders.filter((f) => {
    if (LOADER_EXEMPT.has(f)) return false;
    const src = Deno.readTextFileSync(new URL(f, LIB));
    return !/await mod\.default\([^)]*\)\);?\s*(\/\/[^\n]*\n\s*)*joinChunk\(mod\);/.test(src);
  });
  if (missing.length) throw new Error(`loaders that skip joinChunk: ${missing.join(", ")}`);
});
