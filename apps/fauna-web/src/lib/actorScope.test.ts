import {
  registerActorScopedReset,
  resetActorScopedState,
  registeredActorScopedResetCount,
  sameActorSince,
  actorGeneration,
} from './actorScope.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${actual}, want ${expected}`);
  }
}

// The seam `account-scoping.md` § The scoping taxonomy's in-memory corollary
// requires: registration lives next to the state, and one call drops everything.
Deno.test('registerActorScopedReset — a registered drop runs on reset', () => {
  let dropped = 0;
  const off = registerActorScopedReset(() => { dropped += 1; });
  resetActorScopedState();
  eq(dropped, 1, 'registered reset ran');
  off();
});

Deno.test('registerActorScopedReset — unregister stops it running', () => {
  let dropped = 0;
  const off = registerActorScopedReset(() => { dropped += 1; });
  off();
  resetActorScopedState();
  eq(dropped, 0, 'unregistered reset did not run');
});

Deno.test('registerActorScopedReset — every registration runs, in order', () => {
  const order: string[] = [];
  const offA = registerActorScopedReset(() => order.push('a'));
  const offB = registerActorScopedReset(() => order.push('b'));
  const offC = registerActorScopedReset(() => order.push('c'));
  resetActorScopedState();
  eq(order.join(''), 'abc', 'all three ran in registration order');
  offA(); offB(); offC();
});

// The load-bearing one: a switch that drops only *some* actor-scoped state is the
// silent wrong-actor render this seam exists to prevent, so one throwing reset must
// not strand the rest.
Deno.test('resetActorScopedState — one throwing reset does not strand the others', () => {
  const ran: string[] = [];
  const offA = registerActorScopedReset(() => { ran.push('before'); });
  const offB = registerActorScopedReset(() => { throw new Error('boom'); });
  const offC = registerActorScopedReset(() => { ran.push('after'); });
  resetActorScopedState();
  eq(ran.join(','), 'before,after', 'the reset after the throwing one still ran');
  offA(); offB(); offC();
});

Deno.test('registerActorScopedReset — a repeat registration cannot double-fire', () => {
  let dropped = 0;
  const fn = () => { dropped += 1; };
  const off1 = registerActorScopedReset(fn);
  const off2 = registerActorScopedReset(fn);
  resetActorScopedState();
  eq(dropped, 1, 'the same function registered twice fires once');
  off1(); off2();
});

Deno.test('resetActorScopedState — idempotent, and safe with nothing registered', () => {
  eq(registeredActorScopedResetCount(), 0, 'the suite left no registrations behind');
  resetActorScopedState();
  resetActorScopedState();
  eq(registeredActorScopedResetCount(), 0, 'still empty');
});

// ── The seam before the drop ─────────────────────────────────────────────────
//
// `account-scoping.md`'s in-memory corollary again, its SECOND rule: the loops
// that write actor-scoped state must be retired by the same drop, and a loop
// with no cancellation handle needs a generation the drop bumps. Web's loops
// are the rails' async singleton builders — nothing about them is cancellable —
// so these pin the primitive all three check. Adoption is pinned structurally
// by `actor-generation-contract.test.ts`.

Deno.test('sameActorSince — true while the actor has not changed', () => {
  const still = sameActorSince();
  eq(still(), true, 'no drop has run');
  eq(still(), true, 'and asking twice does not change that');
});

Deno.test('sameActorSince — false once the drop has run', () => {
  const still = sameActorSince();
  resetActorScopedState();
  eq(still(), false, 'the actor this work was started for is gone');
});

// The one that carries the bug: a build captured before the switch, resolving
// after it, must read false even though a NEW build has since started and is
// perfectly current. Both predicates coexist; they are not a single flag.
Deno.test('sameActorSince — the departing build reads false while the incoming one reads true', () => {
  const departing = sameActorSince();
  resetActorScopedState();
  const incoming = sameActorSince();
  eq(departing(), false, "the departing actor's in-flight build must not write");
  eq(incoming(), true, "the incoming actor's build must");
});

// Repeat switches are ordinary (add-account, sign-out, the e2e session patch),
// so the generation must keep moving rather than toggling.
Deno.test('sameActorSince — a second switch does not resurrect the first build', () => {
  const first = sameActorSince();
  resetActorScopedState();
  resetActorScopedState();
  eq(first(), false, 'still false after two switches');
});

Deno.test('actorGeneration — one bump per drop, whatever is registered', () => {
  const before = actorGeneration();
  resetActorScopedState();
  eq(actorGeneration(), before + 1, 'a drop with nothing registered still bumps');

  const off = registerActorScopedReset(() => {});
  resetActorScopedState();
  eq(actorGeneration(), before + 2, 'and a drop with a registration bumps once');
  off();
});

// A throwing reset must not strand the bump any more than it strands the other
// drops: the generation is what the in-flight builds read, so losing it would
// leave them writing as the departed actor.
Deno.test('actorGeneration — a throwing drop does not lose the bump', () => {
  const before = actorGeneration();
  const off = registerActorScopedReset(() => {
    throw new Error('boom');
  });
  resetActorScopedState();
  eq(actorGeneration(), before + 1, 'bumped despite the throwing drop');
  off();
});
