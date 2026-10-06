/**
 * The offline-affordance gate — W4 (account-data-plane.md § Workstreams) phase 4 on web.
 *
 * The charter's class-3 sentence ("UI desensitizes these offline",
 * `docs/goal/architecture/account-data-plane.md` § The offline-mutation
 * contract → *How a surface asks*) as one seam. The *decision* is not ours: it
 * is the shared `fauna_protocol::offline_class::affordance`, reached through
 * the `offlineAffordance` wasm face, which all seven apps read so that none
 * keeps a per-app list of widgets-to-grey (priority #2). Web must never test a
 * class in TypeScript — the rule makes three rulings that are easy to get
 * backwards (only class 3 desensitizes; an *unregistered* kind stays
 * available; only the *known* offline words count as offline). What is web's
 * own is only **how a reactive component tree obeys the verdict**.
 *
 * # Why the SPA can copy neither tui's nor linux's seam
 *
 * tui rebuilds its element list every frame, so it gates in the one place that
 * list is produced (`App::page_elements`) and a stale gate is impossible by
 * construction. **The SPA has no such place.** Its actuable elements are
 * hand-written `<button>` markup spread across ~20 route files and ~36
 * components; there is no single list-building pass to wrap, and inventing one
 * would mean rewriting every call site through a `<GatedButton>` wrapper — a
 * per-app component layer the other six apps do not have.
 *
 * linux registers per widget and re-decides on a `notify::sensitive`, because
 * GTK widgets outlive the state that gated them. Svelte's do not: `disabled=`
 * is an effect that re-runs whenever its dependency changes, so the page would
 * rewrite the property underneath any registry we kept — and a
 * `MutationObserver` racing that write has exactly linux's echo-ambiguity
 * problem (a `true` we wrote and a `true` the page wrote are indistinguishable
 * by value) with none of linux's ordering guarantees to lean on.
 *
 * So the seam here is a **Svelte action that takes the call site's own
 * predicate as a parameter** — the same per-element declaration apple makes
 * with `.faunaGate(kind)` and linux with `declare_wire_kind`, but with the
 * composition made explicit instead of reverse-engineered:
 *
 * ```svelte
 * const offlineGate = makeOfflineGate(offlineAffordance, connectionStatus.subscribe);
 * ...
 * <button use:offlineGate={{ kind: 'fauna.pair.add', disabled: busy }}>
 * ```
 *
 * The call site hands over the `disabled=` it would otherwise have written, so
 * **this action is the property's single writer**. There is no observer, no
 * echo to disambiguate, and no way for two writers to interleave: the bug class
 * linux had to solve does not exist here, because the second writer was removed
 * rather than arbitrated with.
 *
 * # What it never does
 *
 * It never *enables* what the call site disabled. The page's own reason is
 * stronger and more specific than "no nest" — the rule tui's early return and
 * linux's registry both state — so the effective state is
 * `the call site's own intent OR the gate's verdict`, and a reconnect restores
 * exactly the call site's intent, never more.
 *
 * It never gates on a verdict it could not reach. `offlineAffordance` runs in
 * wasm, which the root layout initializes asynchronously; a control rendered
 * before `ensureWasm()` resolves would otherwise be greyed for a reason that
 * has nothing to do with the user's connection. On any throw the gate **fails
 * open** — the polarity ruling 3 chose for an unrecognised state word, and for
 * the same reason: for a gate the honest answer is "do not block the user",
 * and at worst the control shows the error it would have shown anyway.
 *
 * The reason rides the control's `title` — web's native
 * disabled-with-a-reason idiom, and the analogue of linux's tooltip and
 * apple's `.help` — and only when the call site left it empty; it is withdrawn
 * on reconnect only if this gate is what put it there. It is per affordance,
 * never a global "you are offline" banner (§ R11), which is also why one
 * mechanism covers the nest-*less* account.
 *
 * # Why the dependencies are injected
 *
 * Neither `./wasm` nor `./store` may be imported here: both pull the SvelteKit
 * `$app/paths` alias, which plain `deno test` cannot resolve. Taking the
 * affordance fn and the connection-state subscription as parameters keeps this
 * module's own composition rule testable without a wasm/SvelteKit runtime
 * (`offline-gate.test.ts`) — the same seam `family-approvals.ts` documents for
 * the same reason. The production binding is two names at each call site.
 */
import { resolveLocalized, type LocalizedText } from './i18n/localized.ts';

/** `fauna_protocol::offline_class::Affordance` as the wasm face serializes it:
 *  `reason` is present exactly when `available` is false. */
export interface Affordance {
  available: boolean;
  reason?: LocalizedText;
}

/** The `offlineAffordance` wasm face's shape (`$lib/wasm`). */
export type AffordanceFn = (kind: string, connectionState: string) => Affordance;

/** `connectionStatus.subscribe` (`$lib/store`) — the ONE place web learns the
 *  transport state, so the `connection-status` indicator and this gate cannot
 *  disagree about what "connected" means. */
export type StateSubscribe = (run: (state: string) => void) => () => void;

/** What a call site declares. `kind` is the wire kind the control issues —
 *  `null` for a control that issues none (pure local UI: a cancel, a reveal),
 *  worth writing explicitly wherever a reader would expect a kind. `disabled`
 *  is the predicate the call site would otherwise have put in its own
 *  `disabled=`, handed over so this action is that property's only writer. */
export interface OfflineGateParams {
  kind: string | null;
  disabled?: boolean;
}

/** A control this gate can desensitize — every gated element today is a
 *  `<button>`. Structural rather than `HTMLElement` so the rule stays testable
 *  without a DOM, and narrow so it cannot quietly grow a second writer. */
export interface GateableNode {
  disabled?: boolean;
  title: string;
  removeAttribute(name: string): void;
  setAttribute(name: string, value: string): void;
}

/** Stamped on every node this action binds to, regardless of `kind` (even a
 *  `kind: null` binding is a DECLARED "no gate needed", not "never
 *  considered") — the web twin of apple's `isEnabled` closure REGISTRATION
 *  (as opposed to what it currently evaluates to) and linux's
 *  `declare_wire_kind`. `GET /registry`'s `declares_enabled` field
 *  (`web-bridge/server.py`) reads this attribute rather than guessing from
 *  tag type, so it answers "did a call site's own predicate ever reach this
 *  element" — not "is this tag capable of having one" — matching the apple
 *  hazard this distinction exists to catch (`account-data-plane.md` § Built
 *  — the apple leg: a MISSING declaration reads identically to a TRUE one
 *  unless something asks whether it was declared at all).
 */
export const GATE_DECLARED_ATTR = 'data-offline-gate-declared';

/**
 * Build the `use:offlineGate` action, bound to the shared verdict and to web's
 * live connection state.
 *
 * @param affordance the `offlineAffordance` wasm face — never a class test
 *        written here (see the module docs).
 * @param subscribe `connectionStatus.subscribe`.
 */
export function makeOfflineGate(affordance: AffordanceFn, subscribe: StateSubscribe) {
  return function offlineGate(node: GateableNode, params: OfflineGateParams) {
    let current = params;
    let state = 'disconnected';
    /** True while the `title` on this node is the gate's reason text (the call
     *  site had none), so releasing clears only what the gate wrote. */
    let titleIsOurs = false;

    function apply(): void {
      // Idempotent — cheap to set on every call, and `GET /registry` needs it
      // present the instant the node is registered, not only after the first
      // real gate evaluation.
      node.setAttribute(GATE_DECLARED_ATTR, 'true');

      let gated = false;
      let reason = '';

      if (current.kind) {
        try {
          const verdict = affordance(current.kind, state);
          gated = !verdict.available;
          if (gated && verdict.reason) reason = resolveLocalized(verdict.reason);
        } catch {
          gated = false; // fail open — see the module docs.
        }
      }

      node.disabled = Boolean(current.disabled) || gated;

      if (gated && reason && (titleIsOurs || !node.title)) {
        node.title = reason;
        titleIsOurs = true;
      } else if (!gated && titleIsOurs) {
        node.removeAttribute('title');
        titleIsOurs = false;
      }
    }

    const unsubscribe = subscribe((next) => {
      state = next;
      apply();
    });

    return {
      update(next: OfflineGateParams) {
        current = next;
        apply();
      },
      destroy() {
        unsubscribe();
      },
    };
  };
}
