// Behavioural cover for `conversationsDisplayError` — the conversations page's
// `error-message` precedence (`ui/conversations.md` § Errors & edge cases),
// extracted from the page's own `<script>` so it is unit-testable directly.
//
// The case this exists to pin: `pageError` is a plain `string` (Svelte
// `$state('')`), never nullable, so `pageError ?? unopenableMail` can never
// fall through past it to the floor arm — `'' ?? x` is `''`, not `x`. A
// draft shipped exactly that `??` chain; a reviewer caught it before it
// landed. `||` is what actually falls through an empty string.

import { assertEquals } from "jsr:@std/assert";
import { conversationsDisplayError } from "./conversations-display-error.ts";

const NONE = {
  roleRefusal: null,
  receiveStopped: null,
  membershipError: null,
  sendFailure: null,
  pageError: "",
  unopenableMail: null,
};

Deno.test("nothing standing renders nothing", () => {
  assertEquals(conversationsDisplayError(NONE), null);
});

Deno.test("an empty pageError falls through to the unopenable-mail floor arm", () => {
  // The trap: `'' ?? x` is `''`, not `x` — only `||` reaches the floor arm
  // past an empty (not null) pageError.
  assertEquals(
    conversationsDisplayError({ ...NONE, unopenableMail: "3 could not be opened" }),
    "3 could not be opened",
  );
});

Deno.test("a real pageError outranks the unopenable-mail floor arm", () => {
  assertEquals(
    conversationsDisplayError({
      ...NONE,
      pageError: "still loading",
      unopenableMail: "3 could not be opened",
    }),
    "still loading",
  );
});

Deno.test("a fresh send failure outranks the floor arm and an empty pageError", () => {
  assertEquals(
    conversationsDisplayError({
      ...NONE,
      sendFailure: "send failed",
      unopenableMail: "3 could not be opened",
    }),
    "send failed",
  );
});

Deno.test("a membership error outranks a send failure", () => {
  assertEquals(
    conversationsDisplayError({
      ...NONE,
      membershipError: "membership failed",
      sendFailure: "send failed",
    }),
    "membership failed",
  );
});

Deno.test("a stalled receive rail outranks a membership error", () => {
  assertEquals(
    conversationsDisplayError({
      ...NONE,
      receiveStopped: "receive stopped",
      membershipError: "membership failed",
    }),
    "receive stopped",
  );
});

Deno.test("the engine-role refusal outranks everything, including a stalled receive rail", () => {
  assertEquals(
    conversationsDisplayError({
      ...NONE,
      roleRefusal: "served elsewhere",
      receiveStopped: "receive stopped",
      unopenableMail: "3 could not be opened",
    }),
    "served elsewhere",
  );
});
