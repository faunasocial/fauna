// A failed export is a record, not a run (`docs/goal/behavior/mail-export.md`
// § A failed export is a record the wizard shows until the user moves on): the
// Progress step offers none of Pause / Resume / Cancel and one control,
// Dismiss — `mail-export-discard-button` under `mail_export.dismiss_failed_button`.
// `MailExportSection.svelte` branches on this; tui's twin is
// `progress_step_elements`' `failed` flag.

/** The shared snapshot's `session_state` serde string for a failed export. */
export function isFailedExport(sessionState: string | null | undefined): boolean {
  return sessionState === 'Errored';
}
