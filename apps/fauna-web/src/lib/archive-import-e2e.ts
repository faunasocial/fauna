// The archive-import page's e2e commands — `archive_import_pause_after` and
// `archive_import_hold_after`, web's twin of the native bridges' arms over
// `ArchiveImportMachine::set_{pause,hold}_after_records_for_test`
// (`tests/e2e-unified/actions/archive_import.py`, convention 14's causal anchors).
//
// This module exists ONLY in builds made for testing (testing.md § convention 15).
// Two importers, both gated: `#lib/e2e-automation` registers the commands, and the
// archive-import page hands over its machine inside its own
// `if (__FAUNA_E2E_AUTOMATION__)` branch. The setters exist only in the core chunk's
// `test-helpers` flavor (`wasm-core-test`).

import { registerE2eCommands } from '#lib/e2e-commands';
import type { ArchiveImportMachine } from '#lib/wasm-archive-import';

/** The mounted page's machine; `null` when the page is not open. */
let machine: ArchiveImportMachine | null = null;

export function setArchiveImportMachine(m: ArchiveImportMachine | null): void {
  machine = m;
}

const ARCHIVE_IMPORT_COMMANDS = ['archive_import_pause_after', 'archive_import_hold_after'] as const;

interface AnchorSetters {
  setPauseAfterRecordsForTest(records: number): void;
  setHoldAfterRecordsForTest(records: number): void;
}

/** Register both anchors. Called once from `#lib/e2e-automation`. */
export function registerArchiveImportCommands(): void {
  registerE2eCommands(ARCHIVE_IMPORT_COMMANDS, async (action, p) => {
    if (!machine) {
      // Loud, per convention 11: an anchor armed on no machine would let the
      // run finish and the next assertion blame the product.
      throw new Error(`${action}: the archive-import page is not mounted. Navigate to it first.`);
    }
    const setters = machine as unknown as Partial<AnchorSetters>;
    const records = Number(p.records ?? 0);
    const set =
      action === 'archive_import_pause_after'
        ? setters.setPauseAfterRecordsForTest
        : setters.setHoldAfterRecordsForTest;
    if (typeof set !== 'function') {
      throw new Error(
        `${action}: the core chunk in static/ is the production flavor ` +
          '(no test-helpers) — build it with `just web-test`.',
      );
    }
    set.call(machine, records);
    return null;
  });
}
