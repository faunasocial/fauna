// `ensureConnected` (`rpc.ts`) must fail fast on a stopped reconnect loop
// instead of polling out its full 15 s `CONNECT_TIMEOUT_MS` — the SPA-side
// half of `transport-connection.md` § Connection lifecycle's claim that the
// wasm client's `supervisorStop()` record reaches "a page's call ... at once
// too". No web or e2e test mentioned
// `supervisorStop` before this file.
//
// `ensureConnected` takes a real `WsRpcClient` (a wasm binding), which this
// Deno unit-test environment has no way to construct or fake — so, like
// `client-retire-drop-contract.test.ts`, this reads the function's source as
// TEXT rather than driving it. Crude on purpose (see that file's header):
// aimed at the shape that actually recurs — the fast-fail read getting
// deleted or short-circuited — not a parse.

import { stripComments } from "./source-contract.ts";

const RPC = new URL("./rpc.ts", import.meta.url);

function rpcSource(): string {
  return stripComments(Deno.readTextFileSync(RPC));
}

/** `ensureConnected`'s body, from its declaration to the next top-level
 *  declaration. */
function ensureConnectedBody(src: string): string {
  const start = src.indexOf("async function ensureConnected(");
  if (start < 0) {
    throw new Error(
      "rpc.ts no longer declares `async function ensureConnected(` — this " +
        "contract's anchor is gone; re-point it at whatever now owns the " +
        "connect-wait.",
    );
  }
  const rest = src.slice(start + 1);
  const end = rest.search(/\n(?:export )?(?:async )?function /);
  return end < 0 ? rest : rest.slice(0, end);
}

Deno.test("ensureConnected reads supervisorStop before polling out the deadline", () => {
  const body = ensureConnectedBody(rpcSource());
  if (!body.includes(".supervisorStop()")) {
    throw new Error(
      "ensureConnected no longer calls c.supervisorStop() — a page whose " +
        "reconnect loop has stopped for good (transport-connection.md § " +
        "Connection lifecycle) would poll out its full 15 s " +
        "CONNECT_TIMEOUT_MS instead of failing at once with why.",
    );
  }
});

Deno.test("a recorded stop throws instead of falling through to the poll", () => {
  const body = ensureConnectedBody(rpcSource());
  const stopRead = body.indexOf(".supervisorStop()");
  if (stopRead < 0) return; // the test above already reports this
  const rest = body.slice(stopRead, stopRead + 200);
  if (!/if\s*\([^)]*\)\s*throw\b/.test(rest)) {
    throw new Error(
      "ensureConnected reads supervisorStop() but does not throw on it — the " +
        "read must fail the connect wait at once, not merely observe the " +
        "stop and fall through to the 50ms poll loop.",
    );
  }
});
