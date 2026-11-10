// The launch-currency check (`launch-currency.ts`): a replaced or retired
// launch's verdict is dropped, the held one routes exactly once — the web twin
// of linux's `launch_currency::tests`. Run with `just web-unit-test`.
//
// Red-verify by making `deliver` return `true` unconditionally: the replaced
// and the retired launch's verdicts both read as current.

import { assert, assertEquals } from "jsr:@std/assert";
import {
  begin,
  deliver,
  holds,
  launchVerdicts,
  retireInFlight,
} from "./launch-currency.ts";

Deno.test("a replaced or retired launch is dropped and the held one routes once", () => {
  // The counters are module-wide; every assert below is on a delta.
  const before = launchVerdicts();

  const stale = {};
  begin(stale);
  const newer = {};
  begin(newer);
  assert(!holds(stale), "a replaced launch is no longer held");
  assert(!deliver(stale), "a replaced launch's verdict is dropped");
  assert(deliver(newer), "the held launch's verdict routes");
  assert(!deliver(newer), "a settled launch is held no longer");

  const retired = {};
  begin(retired);
  assert(holds(retired));
  retireInFlight();
  assert(!holds(retired), "a retired launch is no longer held");
  assert(!deliver(retired), "a retired launch's verdict is dropped");

  const after = launchVerdicts();
  assertEquals(
    { routed: after.routed - before.routed, superseded: after.superseded - before.superseded },
    { routed: 1, superseded: 3 },
  );
});
