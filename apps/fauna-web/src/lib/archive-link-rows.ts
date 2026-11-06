// Phase two's Contacts-row paint rule (`archive-import.md` § Linking and
// merging, ruling 8) — pure, so a Deno test pins it without wasm. The view is
// the shared `ArchiveLinkView` (`fauna_archive_import_machine::link`, serde
// shape); no state is computed here, only which of the five
// `contact-archive-*` ids a row shows: the question and its two answers on an
// `Asked` row, the linked line and unlink on a `Linked` one, nothing for
// `Offered`, `Declined` or `Unlinked` — tui's `contacts.rs` paint, verbatim.

export type ArchiveLinkState = 'Asked' | 'Offered' | 'Linked' | 'Declined' | 'Unlinked';

export interface ArchiveLinkView {
  contact: string;
  state: ArchiveLinkState;
  platform: string;
  archived_name: string;
  claimed: number;
  unmatched: number;
}

export interface ArchiveRowParts {
  /** Views whose question the row asks (`contact-archive-link-prompt`, `-yes`, `-no`). */
  asked: ArchiveLinkView[];
  /** Views the row shows linked (`contact-archive-linked`, `contact-archive-unlink`). */
  linked: ArchiveLinkView[];
}

/** What `peerId`'s row paints from `views` — matched case-insensitively, as
 *  tui matches the roster's hex against the view's. */
export function archiveRowParts(views: readonly ArchiveLinkView[], peerId: string): ArchiveRowParts {
  const mine = views.filter((v) => v.contact.toLowerCase() === peerId.toLowerCase());
  return {
    asked: mine.filter((v) => v.state === 'Asked'),
    linked: mine.filter((v) => v.state === 'Linked'),
  };
}
