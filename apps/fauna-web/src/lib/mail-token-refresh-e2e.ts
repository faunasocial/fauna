// The `mail_token_refresh_advance_clock` e2e command — web's twin of tui's
// `automation.rs` arm of the same name and the linux/android agent arms. All of
// them drive the same shared Rust seam
// (`fauna_client_mail_settings::token_refresh`): the `now` the 7-day re-mint
// cadence is measured against and each re-minted token's `issued_at`, never the
// bearer clock.
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15): its one importer, `#lib/e2e-automation`, sits behind `+layout.svelte`'s
// `if (__FAUNA_E2E_AUTOMATION__)`, so a production `vite build` folds it away.
//
// Nothing runs here: the refresh reads the clock on its next pass — the SPA's
// next reconnect, or the daily tick — so the journey's witness is the
// production trigger, not this command.

import { mailTokenRefreshSetClockOffsetForTest } from '#lib/wasm';
import { registerE2eCommands } from '#lib/e2e-commands';

const MAIL_TOKEN_REFRESH_COMMANDS = ['mail_token_refresh_advance_clock'] as const;

/** Register `mail_token_refresh_advance_clock`. Called once from `#lib/e2e-automation`. */
export function registerMailTokenRefreshCommands(): void {
  registerE2eCommands(MAIL_TOKEN_REFRESH_COMMANDS, async (_action, p) => {
    // `0` is the reset every test owes its successor — the offset is
    // module-wide and nothing auto-clears it.
    mailTokenRefreshSetClockOffsetForTest(Number(p.now_offset_secs ?? 0));
    return null;
  });
}
