// Deno test for `consentSetLines` — tui's three card pins are
// the template. Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/consent-sets.test.ts

import { consentSetLines } from './consent-sets.ts';

function assert(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

// The card's set section: identity (NSID verbatim beside the publisher's
// title), the publisher's prose, and EVERY member — the title must never
// stand in for the expansion.
Deno.test('a permission set renders its identity, prose and every member', () => {
  const joined = consentSetLines([
    {
      nsid: 'com.example.calendar.appPerms',
      title: 'Calendar sync',
      details: 'Keeps your calendar in step.',
      member_descriptions: ['Read and write calendar events'],
    },
  ]).join('\n');
  assert(
    joined.includes('com.example.calendar.appPerms'),
    `the set identity must render verbatim: ${joined}`,
  );
  assert(joined.includes('Calendar sync'), joined);
  assert(joined.includes('Keeps your calendar in step.'), joined);
  assert(
    joined.includes('Read and write calendar events'),
    `a set's title must never stand in for its expansion: ${joined}`,
  );
});

// A set that declared no title shows its NSID ALONE — never an invented
// label, never an empty pair of quotes.
Deno.test('a set with no declared title renders its nsid alone', () => {
  const joined = consentSetLines([
    {
      nsid: 'com.example.appPerms',
      title: null,
      details: null,
      member_descriptions: ['Upload image files'],
    },
  ]).join('\n');
  assert(joined.includes('com.example.appPerms'), joined);
  assert(!joined.includes('“”'), `no empty quoted title: ${joined}`);
});

// The overwhelmingly common request names no set: the section contributes
// NOTHING — not even a blank line — so that card paints exactly as before.
Deno.test('a request naming no set paints no set section', () => {
  assert(consentSetLines([]).length === 0, 'an empty set list must produce no lines');
});
