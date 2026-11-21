// How an admin control waits for the read that decides it
// (`docs/goal/behavior/admin.md` § Errors & edge cases, *Controls wait for
// their read*; `docs/goal/ui/README.md` § Offer only what is possible, rules
// 5–7).
//
// The answers are shared Rust over wasm — `MailPolicyMachine::write_possibility`,
// the DAV machines' `toggle_possibility`, `ForwarderMachine::add_possibility`,
// `BridgeApprovalMachine::confirm_possibility` / `begin_confirm` / `read_state`,
// `AdminNatModeMachine::select_possibility` / `save_possibility`, and for a
// page's own read `readPossibility` / `writeAfterReadPossibility`
// (`Possibility::from_read` / `Possibility::for_write`). This module only
// paints an answer, publishes the painted reason for automation, and runs the
// press-time halves the pages share. Like `form-gate.ts` it is free of
// `#lib/wasm`, so plain `deno test` reaches it (`admin-read-gate.test.ts`).
// The tui twins are `admin::action_possibility` / `apply_read_gates`
// (`apps/fauna-tui/src/admin/mod.rs`) and `admin/bridges.rs`.

import { askPossibility, formGate, formRefusal, type FormGate } from './form-gate.ts';
import { resolveLocalized } from './i18n/localized.ts';
import { t } from './i18n/strings.ts';
import type { Possibility } from './possibility.ts';
import type { ReadWord } from './read-gate.ts';

export type { FormGate, Possibility, ReadWord };

/** The painted reason as the element's automation `reason` attribute — put on
 *  the element as `data-reason={reasonAttr(gate)}`, which the web bridge's
 *  `/element/attr?attr=reason` reads through its `data-` fallback
 *  (`drivers/base.py::painted_reason`). `undefined` — the attribute absent —
 *  while the control is live or paints no reason. */
export function reasonAttr(gate: FormGate | string | null | undefined): string | undefined {
  if (gate === null || gate === undefined) return undefined;
  const reason = typeof gate === 'string' ? gate : gate.reason;
  return reason === '' ? undefined : reason;
}

/** The paint of a control whose answer a machine face gives: `formGate` over
 *  the face asked now (`null` — disabled, no reason — while there is no
 *  machine yet or the wasm core is not up). */
export function machineGate(ask: (() => Possibility) | null): FormGate {
  return formGate(ask === null ? null : askPossibility(ask));
}

/** A machine-backed control's answer: the machine's own face once the page
 *  has its machine; before that, the answer for the read that builds it
 *  (`Possibility::from_read` over wasm) — still loading while it is being
 *  built, failed to load once the build (its connect or first read) failed,
 *  so a control is never disabled with no reason beside it while the nest is
 *  unreachable. `null` only while the wasm core itself is not up. */
export function machineAnswer<M>(
  machine: M | null,
  ask: (m: M) => Possibility,
  built: ReadWord,
  readPossibility: (read: ReadWord) => Possibility,
): Possibility | null {
  if (machine !== null) return askPossibility(() => ask(machine));
  return askPossibility(() => readPossibility(built));
}

/** A list section's read line (Bridges' pending and approved sections): the
 *  read's own reason while the list is unread — still loading, or failed to
 *  load — painted under the heading and as its `reason` attr; `null` once the
 *  list was read, when the section shows its rows or its empty text. */
export function listReadReason(
  readPossibility: (read: ReadWord) => Possibility,
  read: ReadWord,
): string | null {
  const answer = askPossibility(() => readPossibility(read));
  if (answer === null) return t.common.still_loading;
  return answer.state === 'blocked' ? resolveLocalized(answer.reason) : null;
}

/** The bridge confirm's press (reject or rotate): mark the armed act in
 *  flight through the machine's `beginConfirm` before the `Confirm` dispatch,
 *  so a second press is refused with `common.in_flight`. Returns the refusal
 *  to put on `error-message` (`''` for an absent confirm, nothing to add), or
 *  `null` when the press may dispatch. */
export function beginConfirmPress(
  machine: { beginConfirm(): boolean; confirmPossibility(): Possibility } | null,
): string | null {
  if (machine === null) return t.common.still_loading;
  if (machine.beginConfirm()) return null;
  return formRefusal(askPossibility(() => machine.confirmPossibility())) ?? '';
}

/** Start a machine dispatch and repaint once its synchronous half has run —
 *  a wasm future's first poll runs on the microtask queued when it was
 *  started, which marks the machine in flight — so the control paints *still
 *  saving* while the change is on its way, then resolve with the dispatch. */
export async function dispatchPainted<T>(start: () => Promise<T>, repaint: () => void): Promise<T> {
  const running = start();
  await Promise.resolve();
  repaint();
  return await running;
}

/** A page's read run one at a time: a call while the read is on its way runs
 *  it once more after it ends (the nest may have come back meanwhile — a read
 *  started against the dead socket would otherwise be the last word), never
 *  two at once. Each read catches its own failure (`readFailed`, the machine's
 *  read state), so a re-read that fails after one succeeded keeps the earlier
 *  answer. */
export function serialRead(read: () => Promise<void>): () => Promise<void> {
  let running: Promise<void> | null = null;
  let again = false;
  return () => {
    if (running !== null) {
      again = true;
      return running;
    }
    running = (async () => {
      try {
        do {
          again = false;
          await read();
        } while (again);
      } finally {
        running = null;
      }
    })();
    return running;
  };
}

/** Run `reread` each time the nest connection comes back: the connection
 *  word moving from any other word to `connected` (never on the word the
 *  subscription opens with — the page's entry read covers that). The
 *  subscription is handed in (`connectionStatus.subscribe`), as
 *  `offline-gate.ts` takes it; returns the unsubscribe. */
export function onReconnect(
  subscribe: (run: (word: string) => void) => () => void,
  reread: () => void,
): () => void {
  let last: string | null = null;
  return subscribe((word) => {
    const was = last;
    last = word;
    if (was !== null && was !== 'connected' && word === 'connected') reread();
  });
}
