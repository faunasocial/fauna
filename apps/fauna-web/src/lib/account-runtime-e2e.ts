// The account runtime's e2e surface — the `account_pump_now` poke
// (`fauna_e2e_agent::ACCOUNT_PUMP_NOW`, convention 14's run-now for the
// account pump), the `account_pump_cycles` accessor its barrier reads
// (`fauna_e2e_agent::ACCOUNT_PUMP_CYCLES_KEY`), and the `device_set_state`
// plane reader (`tests/e2e-unified/helpers/fleet.py`), with its siblings
// `read_marker_state`, `escrow_wrap_coordinates` and `folder_set_custody`
// (`fauna_client_folders::custody::read_held_generations`). Each is one line over
// `fauna-wasm`'s `account_runtime` module, so the pass poked is the
// production pass, the counters are the shared `PumpCyclesView` every hosting
// app publishes, and the reader is the shared
// `fauna_account_plane::account_driver::e2e_readers::device_set_state` every
// native dispatcher answers with.
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15): its sole importer is `#lib/e2e-automation`.

import {
  accountDeviceSetState,
  accountEscrowWrapCoordinates,
  accountFolderSetCustody,
  accountPumpCycles,
  accountPumpNow,
  accountReadMarkerState,
  appOutboxDrainNow,
  setOutboxDrainHold,
} from '#lib/wasm';
import { registerE2eCommands } from '#lib/e2e-commands';

const ACCOUNT_RUNTIME_COMMANDS = [
  'account_pump_now',
  'device_set_state',
  'read_marker_state',
  'escrow_wrap_coordinates',
  'folder_set_custody',
  'app_outbox_drain_now',
  'outbox_drain_hold',
] as const;

/** Register the account runtime's commands and install the
 *  `account_pump_cycles` accessor. Called once from `#lib/e2e-automation`. */
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
    if (action === 'read_marker_state') {
      // Same reader shape as `device_set_state`: a missing id is a loud refusal.
      const id = payload?.channel_id_hex;
      if (typeof id !== 'string') {
        throw new Error('read_marker_state needs a `channel_id_hex` string');
      }
      return accountReadMarkerState(id);
    }
    if (action === 'folder_set_custody') {
      // Same reader shape: the set is named by `channel_id_hex` (a member's
      // form) or `set_name` (an owner's live entry); a mistyped field is a
      // loud refusal, and the shared reader refuses a missing or malformed
      // selector the same way (convention 11) — never "nothing held".
      const name = payload?.set_name;
      const channel = payload?.channel_id_hex;
      if (name !== undefined && typeof name !== 'string') {
        throw new Error('folder_set_custody: `set_name` must be a string');
      }
      if (channel !== undefined && typeof channel !== 'string') {
        throw new Error('folder_set_custody: `channel_id_hex` must be a string');
      }
      return accountFolderSetCustody(name, channel);
    }
    if (action === 'escrow_wrap_coordinates') {
      // Same reader shape, no payload: the recovery-kit let-go journey's
      // staging read (`fleet-scope-reclamation.md` clause (3)(j)).
      return accountEscrowWrapCoordinates();
    }
    if (action === 'app_outbox_drain_now') {
      // The tab's app drain loop, rung now (`fauna_e2e_agent::APP_OUTBOX_DRAIN_NOW`):
      // the shared loop the lend and every reconnect run, for a test that
      // has just released the drain hold. Fire-and-forget; the barrier is
      // the effect.
      await appOutboxDrainNow();
      return null;
    }
    if (action === 'outbox_drain_hold') {
      // The in-process drain hold (`fauna_e2e_agent::OUTBOX_DRAIN_HOLD`): a
      // tab has no environment or filesystem, so the desktop hold file
      // cannot reach it. A missing or non-boolean `held` is a loud refusal
      // (convention 11).
      const held = payload?.held;
      if (typeof held !== 'boolean') {
        throw new Error('outbox_drain_hold needs a boolean `held`');
      }
      await setOutboxDrainHold(held);
      return null;
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
