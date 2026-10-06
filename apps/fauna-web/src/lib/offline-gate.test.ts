// Deno tests for the offline-affordance gate's own composition rule. Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/offline-gate.test.ts
//
// The *verdict* is not tested here and must never be re-derived in TypeScript:
// `fauna_protocol::offline_class::affordance` owns the three rulings (only
// class 3 desensitizes; an unregistered kind stays available; only the known
// offline words count as offline) and is pinned Rust-side. `fakeAffordance`
// below stands in for it so these tests exercise the part that IS web's own —
// how a reactive tree composes that verdict with the call site's own predicate,
// and what happens to the reason text — without loading wasm/`$app/paths`
// (see `offline-gate.ts`'s doc comment on why the deps are injected).

import { makeOfflineGate } from './offline-gate.ts';
import type { Affordance, GateableNode } from './offline-gate.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${JSON.stringify(actual)}, want ${JSON.stringify(expected)}`);
  }
}

/** A `<button>`'s gate-relevant surface, with no DOM in sight. */
function node(
  title = '',
): GateableNode & { disabled: boolean; title: string; attrs: Record<string, string> } {
  return {
    disabled: false,
    title,
    attrs: {},
    removeAttribute(name: string) {
      if (name === 'title') this.title = '';
      delete this.attrs[name];
    },
    setAttribute(name: string, value: string) {
      this.attrs[name] = value;
    },
  };
}

/** Stands in for `fauna_protocol::offline_class::affordance` over wasm: one
 *  online-only kind, everything else available. */
function fakeAffordance(kind: string, state: string): Affordance {
  if (kind === 'fauna.pair.add' && state !== 'connected') {
    return { available: false, reason: { key: 'common.needs_nest' } };
  }
  return { available: true };
}

/** A hand-driven stand-in for `connectionStatus.subscribe`: returns a setter so
 *  a test can flip the state the way the WS-RPC client's callback does. */
function fakeState(initial: string) {
  let push: (s: string) => void = () => {};
  const subscribe = (run: (s: string) => void) => {
    push = run;
    run(initial);
    return () => {};
  };
  return { subscribe, set: (s: string) => push(s) };
}

const NEEDS_NEST = 'Needs a connection to your nest';

Deno.test('an online-only control desensitizes with no nest, and says why on itself', () => {
  const state = fakeState('disconnected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node();
  gate(btn, { kind: 'fauna.pair.add' });
  eq(btn.disabled, true, 'online-only control with no nest');
  eq(btn.title, NEEDS_NEST, 'the reason rides the control, never a banner');
});

Deno.test('the same control is live while the nest IS reachable', () => {
  const state = fakeState('connected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node();
  gate(btn, { kind: 'fauna.pair.add' });
  eq(btn.disabled, false, 'gating a connected control would be the gate over-claiming');
  eq(btn.title, '', 'no reason to show while it is usable');
});

Deno.test('an offline-capable sibling stays live with no nest', () => {
  // The direction a blanket disable fails. Classes 1 and 2 are precisely what
  // works without a nest; greying them strands the user.
  const state = fakeState('disconnected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node();
  gate(btn, { kind: 'fauna.account.state.put' });
  eq(btn.disabled, false, 'an offline-safe kind must not desensitize');
});

Deno.test('a control issuing no wire kind at all is never gated', () => {
  const state = fakeState('disconnected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node();
  gate(btn, { kind: null });
  eq(btn.disabled, false, 'pure local UI (a cancel button) needs no nest');
});

Deno.test('binding the action stamps GET /registry\'s declared-marker, even for kind: null', () => {
  // `kind: null` is a DECLARED "no gate needed", not "never considered" — the
  // web twin of apple's isEnabled closure REGISTRATION (registered-but-always-
  // true still counts). `GET /registry`'s declares_enabled reads this
  // attribute, not the tag, so a plain unbound <button> stays distinguishable
  // from one this action was actually applied to.
  const state = fakeState('disconnected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node();
  eq(btn.attrs['data-offline-gate-declared'], undefined, 'unbound — no marker yet');
  gate(btn, { kind: null });
  eq(btn.attrs['data-offline-gate-declared'], 'true', 'bound, even with no wire kind');
});

Deno.test("the call site's own predicate still disables, and the gate never overrides it", () => {
  const state = fakeState('connected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node();
  gate(btn, { kind: 'fauna.pair.add', disabled: true });
  eq(btn.disabled, true, "the page's own intent is stronger than 'the nest is reachable'");
});

Deno.test('a reconnect restores the page\'s intent, never a blanket enable', () => {
  // The bug linux had to solve with echo-matching: a gate that "releases" by
  // writing `false` would enable a control the page had disabled for its own
  // reason.
  const state = fakeState('disconnected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node();
  const handle = gate(btn, { kind: 'fauna.pair.add', disabled: true });
  eq(btn.disabled, true, 'disabled by both');
  state.set('connected');
  eq(btn.disabled, true, 'the nest came back but the page still says no');
  handle.update({ kind: 'fauna.pair.add', disabled: false });
  eq(btn.disabled, false, 'now that both agree, it is live');
});

Deno.test('the gate closes and reopens as the connection flips', () => {
  const state = fakeState('connected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node();
  gate(btn, { kind: 'fauna.pair.add' });
  eq(btn.disabled, false, 'live to start');
  state.set('disconnected');
  eq(btn.disabled, true, 'closes when the nest goes');
  eq(btn.title, NEEDS_NEST, 'and says why');
  state.set('connected');
  eq(btn.disabled, false, 'a gate that only closes is a stranding, not a gate');
  eq(btn.title, '', 'the reason is withdrawn with the gate');
});

Deno.test('the gate never clobbers a reason the call site wrote', () => {
  const state = fakeState('disconnected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node("this nest is already linked");
  gate(btn, { kind: 'fauna.pair.add' });
  eq(btn.disabled, true, 'still gated');
  eq(btn.title, 'this nest is already linked', "the page's own reason is more specific");
});

Deno.test("the gate withdraws only the title it wrote itself", () => {
  const state = fakeState('disconnected');
  const gate = makeOfflineGate(fakeAffordance, state.subscribe);
  const btn = node('this nest is already linked');
  gate(btn, { kind: 'fauna.pair.add' });
  state.set('connected');
  eq(btn.title, 'this nest is already linked', "the page's tooltip survives the gate opening");
});

Deno.test('a verdict that cannot be reached fails OPEN', () => {
  // wasm is initialized asynchronously by the root layout; a control rendered
  // before `ensureWasm()` resolves must not be greyed for a reason that has
  // nothing to do with the user's connection. Same polarity as ruling 3: for a
  // gate the honest answer is "do not block the user".
  const state = fakeState('disconnected');
  const throws = () => {
    throw new Error('WASM not initialized — call ensureWasm() first');
  };
  const gate = makeOfflineGate(throws, state.subscribe);
  const btn = node();
  gate(btn, { kind: 'fauna.pair.add' });
  eq(btn.disabled, false, 'an unreachable verdict must not desensitize');
  eq(btn.title, '', 'and must not invent a reason');
});

Deno.test('destroy releases the connection-state subscription', () => {
  let released = false;
  const subscribe = (run: (s: string) => void) => {
    run('connected');
    return () => {
      released = true;
    };
  };
  const gate = makeOfflineGate(fakeAffordance, subscribe);
  const btn = node();
  gate(btn, { kind: 'fauna.pair.add' }).destroy();
  eq(released, true, 'a component teardown must not leak a store subscription');
});
