// The `backup_enroll_custodian_for_test` e2e command — a stand-in for ANOTHER
// device of this owner enrolling itself as a client-device custodian
// (`docs/goal/ui/backups.md` § Third destination kind).
//
// This module exists ONLY in builds made for testing (convention 15): its sole
// importer is `$lib/e2e-automation`, itself reached only through
// `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch, which a production
// `vite build` constant-folds away. The wasm face it calls
// (`backupDestinationEnrollCustodianForTest`) is gated on `fauna-wasm`'s
// `test-helpers` feature, so the production wasm flavor does not export it.
//
// ## Why a command, and why it is not web's own gesture
//
// Enrolment runs on the device being enrolled, so web declares the kind select
// and capacity input absent (`backups.md` § Implementation status today) while
// still owing the display: the kind badge, the usage line and the sole-client
// warning. No web gesture can produce a client-device row, so a test that
// wants web to show one needs another device to have enrolled. This command
// is that other device's act, run through the same shared
// `enroll_client_custodian` every enrolling app calls, under a stand-in device
// id. What the test then asserts, what the Backups page renders from the
// destination list it reads back, stays entirely the product's.

import { get } from 'svelte/store';

import { registerE2eCommands } from '$lib/e2e-commands';
import { rpcCall } from '$lib/rpc';
import { identity } from '$lib/store';
import { accountsTabSessionMaterial } from '$lib/accounts';

interface CustodianEnrollSeam {
  backupDestinationEnrollCustodianForTest(
    secretHex: string,
    deviceId: string,
    name: string,
    capacity: string,
  ): Promise<unknown>;
}

const BACKUP_CUSTODIAN_COMMANDS = ['backup_enroll_custodian_for_test'] as const;

/** Register `backup_enroll_custodian_for_test`. Called once from
 *  `$lib/e2e-automation`. Payload: `{device_id: string, name?: string,
 *  capacity?: string}`; a blank capacity is uncapped. */
export function registerBackupCustodianCommands(): void {
  registerE2eCommands(BACKUP_CUSTODIAN_COMMANDS, async (action, p) => {
    const secret = get(identity)?.secretHex ?? accountsTabSessionMaterial()?.secret_hex;
    if (!secret) {
      throw new Error(`${action}: no identity on this seat`);
    }
    const deviceId = typeof p.device_id === 'string' ? p.device_id.trim() : '';
    if (!deviceId) {
      // The shared enroll refuses a blank device id; say so here with the
      // command's own name rather than surfacing its error from deeper down.
      throw new Error(`${action}: device_id is required (the stand-in device's id)`);
    }
    await rpcCall(secret, (c) => {
      const seam = c as unknown as Partial<CustodianEnrollSeam>;
      if (typeof seam.backupDestinationEnrollCustodianForTest !== 'function') {
        throw new Error(
          `${action}: backupDestinationEnrollCustodianForTest is absent — this SPA ` +
            'is running the PRODUCTION wasm flavor; build the test flavor ' +
            '(`just web-test`).',
        );
      }
      return seam.backupDestinationEnrollCustodianForTest(
        secret,
        deviceId,
        typeof p.name === 'string' ? p.name : '',
        typeof p.capacity === 'string' ? p.capacity : '',
      );
    });
    return null;
  });
}
