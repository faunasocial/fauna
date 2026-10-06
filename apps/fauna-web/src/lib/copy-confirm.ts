// The copy-button funnel: write `text` to the clipboard and record the
// displayed confirmation ("Copied!") in the log ring at `info` —
// observability.md § What must be logged, category 1 (the button label swap is
// a displayed success line; the ring is its durable record). The log carries the
// displayed text only, never `text` itself (§ 2's redaction rule: a copied
// value may be a secret). The caller still owns the label swap and its
// `data-copied` attribute, which reports the exact string that was copied.
//
// The clipboard write is best-effort, like `bestEffortCopyToClipboard`
// (`web-publish.ts`): a headless or permission-blocked browser throws — even
// synchronously — and the caller must still record its confirmation. The
// failure is swallowed, so it is logged (category 3), without the value.
import { logMessage } from './wasm';
import { t } from './i18n/strings.ts';

export async function copyAndConfirm(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch (e) {
    logMessage('warn', 'fauna_web::notice', `clipboard write failed: ${e}`);
  }
  logMessage('info', 'fauna_web::notice', t.common.copied);
}
