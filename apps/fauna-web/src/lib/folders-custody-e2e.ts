// The `folders_hold_custody_reads` / `folders_release_custody_reads` e2e
// commands — web's twins of tui's `automation.rs` arms of the same names and
// the linux/android agent arms. All of them drive the same shared Rust gate
// (`fauna_client_folders::webdav_provision::hold_custody_reads_for_test`): held,
// every mail-custody read behind `foldersCanServeWebdav` parks until the
// release, so `folder-webdav-toggle` is read while that read is out
// (`ui/folders.md` § Errors & edge cases, *Own-tier and mail gates* — blocked
// "still loading"). The Folders section starts that read with `void` after its
// rows paint, so nothing here has to start-rather-than-await while held.
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15): its one importer, `#lib/e2e-automation`, sits behind `+layout.svelte`'s
// `if (__FAUNA_E2E_AUTOMATION__)`, so a production `vite build` folds it away.

import { foldersSetCustodyReadsHeldForTest } from '#lib/wasm';
import { registerE2eCommands } from '#lib/e2e-commands';

const FOLDERS_CUSTODY_COMMANDS = [
  'folders_hold_custody_reads',
  'folders_release_custody_reads',
] as const;

/** Register the custody-read hold commands. Called once from `#lib/e2e-automation`. */
export function registerFoldersCustodyCommands(): void {
  registerE2eCommands(FOLDERS_CUSTODY_COMMANDS, async (action) => {
    foldersSetCustodyReadsHeldForTest(action === 'folders_hold_custody_reads');
    return null;
  });
}
