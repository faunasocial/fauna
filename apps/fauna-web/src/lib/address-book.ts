// The Address Book segment's book read on web (`ui/contacts.md` § Errors &
// edge cases): the wasm face `carddavReadAddressbooks` answers in the shared
// `fauna_client_carddav::AddressbooksRead` states, and the sidebar paints from
// that state alone. While the read is unresolved — a missing key included, the
// mint in flight (`behavior/mail-credentials.md` § MSEK lifecycle, ruling 7) —
// the sidebar paints loading, never an empty address book and never a
// credential step; only a loaded read with no book is "No address books yet.".
// The same rule as tui's `address_book::render` and linux's
// `book_placeholder_text`.

/** `carddavReadAddressbooks`'s serde form (the wasm face's value). */
export type AddressbooksRead =
  | { state: 'unresolved' }
  | { state: 'loaded'; addressbooks: any[] };

/** What the book sidebar says when it lists no book, or `null` when it lists
 *  books: `loading` while the read has not answered, `empty` once a loaded
 *  read holds none. */
export function addressBookPlaceholder(read: AddressbooksRead): 'loading' | 'empty' | null {
  if (read.state === 'unresolved') return 'loading';
  return read.addressbooks.length === 0 ? 'empty' : null;
}

/** The books a read holds — none while it is unresolved. */
export function addressBooksOf(read: AddressbooksRead): any[] {
  return read.state === 'loaded' ? read.addressbooks : [];
}
