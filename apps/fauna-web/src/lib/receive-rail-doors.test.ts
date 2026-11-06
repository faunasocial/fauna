// Pins the e2e doors of the dead-receive-rail witness: mark raises the stalled flag the
// conversations page ranks onto `error-message`, begin retires it again.

import { assertEquals } from "jsr:@std/assert";
import { beginReceiveLoopForTest, markReceiveStoppedForTest } from "./receive-rail-doors.ts";

function flag() {
  const f = { value: false, set(v: boolean) { f.value = v; } };
  return f;
}

Deno.test("mark stands the stalled flag up and begin retires it", () => {
  const stalled = flag();
  markReceiveStoppedForTest(stalled);
  assertEquals(stalled.value, true);
  beginReceiveLoopForTest(stalled);
  assertEquals(stalled.value, false);
});

Deno.test("begin on a healthy rail is a no-op", () => {
  const stalled = flag();
  beginReceiveLoopForTest(stalled);
  assertEquals(stalled.value, false);
});
