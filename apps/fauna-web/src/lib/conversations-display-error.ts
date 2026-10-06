// The conversations page's `error-message` precedence
// (`ui/conversations.md` § Errors & edge cases), extracted as a pure function
// so it is unit-testable directly rather than only through the page's own
// `<script>` — and kept import-free so `deno test` can load it without
// pulling in `$lib/conversations`'s whole (svelte-store, wasm) module graph.
//
// Order, highest first: the engine-role refusal, a stalled receive rail, a
// membership/label wire-op failure, a failed compose-send, then the page's
// own JS-exception channel — falling through to the unopenable-mail floor
// notice **only when `pageError` is empty**. `pageError` is a plain `string`
// (Svelte `$state('')`), not nullable, so `??` alone would never fall through
// past it to the floor arm — `||` is required for that one step.
export function conversationsDisplayError(inputs: {
  roleRefusal: string | null;
  receiveStopped: string | null;
  membershipError: string | null;
  sendFailure: string | null;
  pageError: string;
  unopenableMail: string | null;
}): string | null {
  return (
    inputs.roleRefusal ??
    inputs.receiveStopped ??
    inputs.membershipError ??
    inputs.sendFailure ??
    (inputs.pageError || inputs.unopenableMail)
  );
}
