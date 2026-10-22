// The mail-import wizard's snapshot-driven controls, kept out of the component
// so a Deno test can pin them (`MailImportSection.svelte` is the only caller;
// tui's twins are `progress_step_elements`, `forget_source_element` and
// `settings::apply_mail_import_snapshot`).

/** A failed import is a record, not a run (`docs/goal/behavior/mailbox-migration.md`
 *  § A failed import is a record the wizard shows until the user moves on): the
 *  Progress step offers none of Pause / Resume / Restart / Cancel and one
 *  control, Dismiss — `mail-import-dismiss-button`. */
export function isFailedImport(sessionState: string | null | undefined): boolean {
  return sessionState === 'Errored';
}

/** The restart offer (§ Resume protocol step 4): shown only while the machine
 *  holds a source mailbox the old provider rebuilt. */
export function restartOffered(rebuilt: string[] | null | undefined): boolean {
  return (rebuilt?.length ?? 0) > 0;
}

export interface RememberedSourceSnapshot {
  source_remembered: boolean;
  host: string;
  port: number;
  username: string;
}

export interface SourceDrafts {
  host: string;
  port: string;
  username: string;
  /** True when the drafts were just emptied by a landed Forget: the typed
   *  password goes with them. */
  clearPassword: boolean;
  /** The new `draftsSeeded` flag. */
  seeded: boolean;
}

/** What the Source drafts become after a snapshot (`mailbox-migration.md`
 *  § Credential handling → *The remembered source*): filled from the
 *  remembered source once per visit and only while the username draft is
 *  empty — never over what the user typed — and emptied when the user's Forget
 *  lands. `null` means leave the drafts alone. */
export function nextSourceDrafts(
  snap: RememberedSourceSnapshot,
  usernameEmpty: boolean,
  seeded: boolean,
  wasRemembered: boolean,
): SourceDrafts | null {
  if (snap.source_remembered && !seeded && usernameEmpty) {
    return {
      host: snap.host,
      port: String(snap.port),
      username: snap.username,
      clearPassword: false,
      seeded: true,
    };
  }
  if (wasRemembered && !snap.source_remembered) {
    return { host: '', port: '', username: '', clearPassword: true, seeded };
  }
  return null;
}
