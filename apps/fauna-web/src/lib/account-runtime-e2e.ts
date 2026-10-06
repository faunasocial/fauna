// The account runtime's e2e surface — the `account_pump_now` poke
// (`fauna_e2e_agent::ACCOUNT_PUMP_NOW`, convention 14's run-now for the
// account pump), the `account_pump_cycles` accessor its barrier reads
// (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`), and the `device_set_state`
// plane reader (`tests/e2e-unified/helpers/fleet.py`). Each is one line over
// `fauna-wasm`'s `account_runtime` module, so the pass poked is the
// production pass, the counters are the shared `PumpCyclesView` every hosting
// app publishes, and the reader is the shared
// `fauna_account_plane::account_driver::e2e_readers::device_set_state` every
// native dispatcher answers with.
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15): its sole importer is `$lib/e2e-automation`.

import { accountDeviceSetState, accountPumpCycles, accountPumpNow } from '$lib/wasm';
import { registerE2eCommands } from '$lib/e2e-commands';

const ACCOUNT_RUNTIME_COMMANDS = ['account_pump_now', 'device_set_state'] as const;

/** Register the account runtime's commands and install the
 *  `account_pump_cycles` accessor. Called once from `$lib/e2e-automation`. */
export function registerAccountRuntimeCommands(): void {
  registerE2eCommands(ACCOUNT_RUNTIME_COMMANDS, async (action, payload) => {
    if (action === 'device_set_state') {
      // An on-demand reader (async store I/O), never a state-blob key — the
      // same shape as every native dispatcher's arm. A built reader always
      // answers an object, at minimum `{"found": false}`. A missing or
      // mistyped id is a loud refusal naming the field (convention 11) —
      // never folded into a not-found, which would make "the row is NOT
      // there" pass vacuously on a typo.
      const id = payload?.device_id_hex;
      if (typeof id !== 'string') {
        throw new Error('device_set_state needs a `device_id_hex` string');
      }
      return accountDeviceSetState(id);
    }
    // Fire-and-forget by contract: the barrier is `account_pump_cycles`,
    // never this ack (the pass itself may outlast a command's budget).
    void accountPumpNow();
    return null;
  });
  (
    window as unknown as { __fauna_accountPumpCycles?: typeof accountPumpCycles }
  ).__fauna_accountPumpCycles = accountPumpCycles;
}
