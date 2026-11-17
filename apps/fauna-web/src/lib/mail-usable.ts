// The session's ONE `WasmMailSettingsMachine` and the live mail-usable answer
// read off it (`docs/goal/ui/mail-settings.md` § Errors & edge cases, the
// mail-usable gate).
//
// The Settings shell's rail and the six mail sub-pages paint from
// `mailUsable`; the Mail & Calendar page drives the same machine
// (`sharedMailSettingsMachine`), so turning mail on there lights the rows at
// once — the rail reads the machine's LIVE snapshot, never a copy. The shell
// starts the watch when it mounts, so the answer is decided (the machine
// hydrated) before the person reaches the rail. A machine that fails to build
// answers the shared unbuilt reason (rule 7), never silent controls.

import { writable, type Writable } from 'svelte/store';
import { mailSettingsMachine } from './rpc';
import { ensureWasm, mailUsableUnbuilt, mailUsableUnread } from './wasm';
import { registerActorScopedReset } from './actorScope';
import type { Possibility } from './mail-gate';
import type { WasmMailSettingsMachine } from '../../static/fauna_wasm.js';

/** The answer the rail and the sub-pages paint. From the moment the wasm core
 *  loads under a mounted shell and until the machine's first read it is the
 *  shared `mailUsableUnread()` answer (blocked, "checking" — rule 5: unknown is
 *  not possible); `null` only before that, which `mail-gate.ts` paints as
 *  blocked with no reason yet — the SPA never spells a reason itself. */
export const mailUsable: Writable<Possibility | null> = writable(null);

let shared: { secretHex: string; machine: Promise<WasmMailSettingsMachine> } | null = null;

registerActorScopedReset(() => {
  shared = null;
  mailUsable.set(null);
});

/** The session's one mail-settings machine for this actor, built (and
 *  hydrated) once. Every page that reads or drives mail settings takes this. */
export function sharedMailSettingsMachine(secretHex: string): Promise<WasmMailSettingsMachine> {
  if (shared?.secretHex !== secretHex) {
    const machine = (async () => {
      await ensureWasm();
      const m = await mailSettingsMachine(secretHex);
      await m.hydrate().catch(() => {});
      return m;
    })();
    shared = { secretHex, machine };
    machine.catch(() => {
      if (shared?.machine === machine) shared = null;
    });
  }
  return shared.machine;
}

/** Keep `mailUsable` current while the Settings shell is mounted; returns the
 *  stop. Re-reads the live snapshot on a short tick (the linux shell's own
 *  shape), publishing only a changed answer. */
export function watchMailUsable(secretHex: string): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setInterval> | null = null;
  let last = '';
  const publish = (answer: Possibility) => {
    const key = JSON.stringify(answer);
    if (key !== last) {
      last = key;
      mailUsable.set(answer);
    }
  };
  ensureWasm().then(
    () => {
      if (!stopped && last === '') publish(mailUsableUnread());
    },
    () => {},
  );
  sharedMailSettingsMachine(secretHex).then(
    (machine) => {
      if (stopped) return;
      const read = () => publish(machine.mailUsable() as Possibility);
      read();
      timer = setInterval(read, 400);
    },
    async () => {
      if (stopped) return;
      await ensureWasm().catch(() => {});
      publish(mailUsableUnbuilt());
    },
  );
  return () => {
    stopped = true;
    if (timer !== null) clearInterval(timer);
  };
}
