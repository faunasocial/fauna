// The `mail-import` wizard's Confirm summary line (`mailbox-migration.md`
// § Wizard steps step 4). The sum comes from the shared
// `MailImportSnapshot::selected_totals` (over wasm: `importSelectedTotals`) —
// never re-summed here; this only picks the sized or unsized template.

export interface ImportTotals {
  mailboxes: number;
  messages: number;
  /** `null` unless every selected mailbox was sized. */
  bytes: number | null;
}

export interface ConfirmTemplates {
  plain: (v: { source: string; mailboxes: string; messages: string }) => string;
  sized: (v: { source: string; mailboxes: string; messages: string; size: string }) => string;
}

/** The sized template when the estimate carries bytes, else the count-only one. */
export function confirmSummaryText(
  source: string,
  totals: ImportTotals,
  templates: ConfirmTemplates,
  byteSize: (bytes: number) => string,
): string {
  const v = {
    source,
    mailboxes: String(totals.mailboxes),
    messages: String(totals.messages),
  };
  return totals.bytes === null
    ? templates.plain(v)
    : templates.sized({ ...v, size: byteSize(totals.bytes) });
}
