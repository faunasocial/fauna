// Deno tests for the Address Book sidebar's placeholder (`ui/contacts.md`
// § Errors & edge cases). Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/address-book.test.ts

import { addressBookPlaceholder, addressBooksOf } from './address-book.ts';

function assert(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

Deno.test('an unresolved read (a missing key) paints loading, never the empty book', () => {
  assert(addressBookPlaceholder({ state: 'unresolved' }) === 'loading', 'unresolved must paint loading');
  assert(addressBooksOf({ state: 'unresolved' }).length === 0, 'unresolved holds no book');
});

Deno.test('a loaded read with no book paints the empty address book', () => {
  assert(addressBookPlaceholder({ state: 'loaded', addressbooks: [] }) === 'empty', 'loaded + none must paint empty');
});

Deno.test('a loaded read with books paints no placeholder', () => {
  const read = { state: 'loaded' as const, addressbooks: [{ id: 'aa', name: 'Personal' }] };
  assert(addressBookPlaceholder(read) === null, 'books listed: no placeholder');
  assert(addressBooksOf(read).length === 1, 'the books pass through');
});
