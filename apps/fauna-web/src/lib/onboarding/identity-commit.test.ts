// Deno tests for moment 1's failure path. Run via:
//
//     just web-unit-test
//
// `long-term-store.md` § Save-failure handling: a rejected registry write is
// reported to the wizard machine (`identityCommitFailed`), which returns to the
// confirm screen with the error; a successful write reports nothing.
import { assert, assertEquals } from "jsr:@std/assert";
import { commitConfirmedIdentity } from "./identity-commit.ts";

Deno.test("a rejected persist reports the wrapped error to the machine", async () => {
  const reported: string[] = [];
  const logged: unknown[] = [];
  await commitConfirmedIdentity("ab".repeat(32), false, {
    persist: () => Promise.reject(new Error("storage is full")),
    commitFailed: (m) => reported.push(m),
    log: (e) => logged.push(e),
  });
  assertEquals(reported, ["Couldn't save your new identity: storage is full"]);
  assertEquals(logged.length, 1);
});

Deno.test("a shared refusal line is the message, wrapped", async () => {
  const reported: string[] = [];
  await commitConfirmedIdentity("ab".repeat(32), false, {
    persist: () => Promise.reject("the account list is full"),
    commitFailed: (m) => reported.push(m),
    log: () => {},
  });
  assertEquals(reported.length, 1);
  assert(reported[0].includes("the account list is full"));
});

Deno.test("a successful persist reports nothing and passes append through", async () => {
  const reported: string[] = [];
  const calls: Array<[string, boolean]> = [];
  await commitConfirmedIdentity("cd".repeat(32), true, {
    persist: (s, a) => {
      calls.push([s, a]);
      return Promise.resolve("actor");
    },
    commitFailed: (m) => reported.push(m),
    log: () => {},
  });
  assertEquals(reported, []);
  assertEquals(calls, [["cd".repeat(32), true]]);
});
