// Deno test for web's half of the mail export confirm step's two estimates
// (`docs/goal/behavior/mail-export.md` § Wizard steps step 3). The figures come
// from ONE shared computation, `fauna_client_mail_settings::export_confirm_estimate`
// (WASM `exportConfirmEstimate`); `MailExportSection.svelte`'s `confirmSummary`
// folds them into `mail-export-confirm-summary` through
// `mail_export.confirm_summary_estimated_fmt`, and keeps the bare counts line
// plus `confirm_estimate_unavailable` when selected messages carry zero bytes.
// tui's twin is `apps/fauna-tui/src/settings/mail_export.rs`'s
// `the_confirm_step_estimates_the_selected_bytes_and_time`.
//
// Like `nests-grant-scope-labels.test.ts`, this loads the static wasm-pack
// bundle directly and degrades to a skip when that gitignored build artifact is
// absent or predates the export.
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`
// (the project's `web-unit-test` recipe already covers this directory).

import { assert, assertEquals } from 'jsr:@std/assert';
import { resolveLocalized, type LocalizedText } from './i18n/localized.ts';
import { t } from './i18n/strings.ts';

const wasmModuleUrl = new URL('../../static/fauna_wasm.js', import.meta.url);
const wasmBinaryUrl = new URL('../../static/fauna_wasm_bg.wasm', import.meta.url);
const componentUrl = new URL('./components/MailExportSection.svelte', import.meta.url);

type Option = { name: string; selected: boolean; message_count: number; sealed_bytes: number };
type Estimate = { messages: number; sealed_bytes: number; secs: number };
let mod:
  | {
    exportConfirmEstimate: (mailboxes: Option[]) => Estimate;
    byteSize: (bytes: number) => LocalizedText;
    durationSecs: (secs: number) => LocalizedText;
  }
  | undefined;
try {
  mod = await import(wasmModuleUrl.href);
  await (mod as unknown as { default: (o: unknown) => Promise<unknown> }).default({
    module_or_path: await Deno.readFile(wasmBinaryUrl),
  });
  // A bundle built before the export existed is as absent as no bundle.
  if (typeof mod?.exportConfirmEstimate !== 'function') mod = undefined;
} catch {
  mod = undefined;
}

const option = (name: string, selected: boolean, sealed_bytes: number): Option => ({
  name,
  selected,
  message_count: 10,
  sealed_bytes,
});

Deno.test({
  name: 'the confirm estimate counts only the selected mailboxes, bytes and time included',
  ignore: mod === undefined,
  fn: () => {
    const estimate = mod!.exportConfirmEstimate([
      option('INBOX', true, 150 << 20),
      option('Trash', false, 900 << 20),
    ]);
    // 150 MiB at the shared 1 MiB/s is 150 s.
    assertEquals(estimate, { messages: 10, sealed_bytes: 150 << 20, secs: 150 });
    const summary = t.mail_export.confirm_summary_estimated_fmt({
      summary: 'counts',
      bytes: resolveLocalized(mod!.byteSize(estimate.sealed_bytes)),
      duration: resolveLocalized(mod!.durationSecs(estimate.secs)),
    });
    assertEquals(summary, 'counts · about 150 MB · about 2m to export');
  },
});

Deno.test({
  name: 'a nest reporting no sizes leaves zero bytes, so the section says "not estimated"',
  ignore: mod === undefined,
  fn: () => {
    const estimate = mod!.exportConfirmEstimate([option('INBOX', true, 0)]);
    assertEquals(estimate.messages, 10);
    assertEquals(estimate.sealed_bytes, 0);
  },
});

// The section takes every figure from the shared function and sums nothing
// itself (priority #2): a hand-summed `message_count` reappearing is the drift
// this row retired.
Deno.test('MailExportSection takes the confirm figures from exportConfirmEstimate', async () => {
  const src = await Deno.readTextFile(componentUrl);
  assert(src.includes('exportConfirmEstimate(snap.mailboxes)'));
  assert(!/reduce\([^)]*message_count/.test(src), 'the section must not sum message_count itself');
  assert(src.includes('confirm_summary_estimated_fmt'));
});
