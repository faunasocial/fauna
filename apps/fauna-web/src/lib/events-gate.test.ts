// The Events page's write gates at the unit tier (`docs/goal/ui/events.md`
// § Errors & edge cases → *No calendar to hold an event*, rulings 3 and 4, and
// → *The other calendar writes*). Two halves:
//
//  1. The helpers (`#lib/events-gate`): what each predicate answer makes a door
//     or an action say, the no-key refusal at the act, the failed save's words
//     and the unreadable-calendar notice.
//  2. A source contract over the page (testing.md convention 17): every door to
//     the compose asks the event-create predicate at the press, every other
//     calendar write asks the calendar-write predicate before it sends, the
//     import's no-file refusal comes after that ask, and a failed save lands on
//     the form's own `error-message`.
//
// Run via `just web-unit-test`, or:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/events-gate.test.ts

import { assert, assertEquals } from 'jsr:@std/assert';
import {
  calendarWriteFailure,
  errorText,
  LOADING_REASON,
  NO_KEY_REFUSAL,
  refusalOf,
  unreadableNotice,
} from './events-gate.ts';
import { stripComments } from './source-contract.ts';

const PROVISION = {
  key: 'events.create_blocked.provision_failed',
  args: { error: 'refused' },
};

Deno.test('refusalOf — a possible answer opens the door', () => {
  assertEquals(refusalOf({ state: 'possible' }), null);
});

Deno.test('refusalOf — a blocked answer refuses with its own reason', () => {
  assertEquals(refusalOf({ state: 'blocked', reason: LOADING_REASON }), LOADING_REASON);
  assertEquals(refusalOf({ state: 'blocked', reason: PROVISION }), PROVISION);
});

Deno.test('refusalOf — an absent answer refuses as loading, never sends', () => {
  assertEquals(refusalOf({ state: 'absent' }), LOADING_REASON);
});

Deno.test('calendarWriteFailure — the no-key refusal at the act says loading', () => {
  // A rejected wasm promise carries a bare string; a wrapped one an Error.
  assertEquals(calendarWriteFailure(NO_KEY_REFUSAL, 'fallback'), LOADING_REASON);
  assertEquals(calendarWriteFailure(new Error(NO_KEY_REFUSAL), 'fallback'), LOADING_REASON);
});

Deno.test('calendarWriteFailure — any other failure keeps its words, else the fallback', () => {
  assertEquals(calendarWriteFailure(new Error('nest said no'), 'fallback'), 'nest said no');
  assertEquals(calendarWriteFailure('wire closed', 'fallback'), 'wire closed');
  assertEquals(calendarWriteFailure(new Error(''), 'fallback'), 'fallback');
});

Deno.test('errorText — reads an Error, a string and a message-bearing object', () => {
  assertEquals(errorText(new Error('a')), 'a');
  assertEquals(errorText('b'), 'b');
  assertEquals(errorText({ message: 'c' }), 'c');
  assertEquals(errorText(null), '');
});

Deno.test('unreadableNotice — a loaded read with calendars left out says how many', () => {
  assertEquals(unreadableNotice({ state: 'loaded', unreadable: 2 }), {
    key: 'events.calendars_unreadable',
    args: { count: '2' },
  });
});

Deno.test('unreadableNotice — nothing to say when every calendar read, or the read is not loaded', () => {
  assertEquals(unreadableNotice({ state: 'loaded', unreadable: 0 }), null);
  assertEquals(unreadableNotice({ state: 'loaded' }), null);
  assertEquals(unreadableNotice({ state: 'unresolved' }), null);
  assertEquals(unreadableNotice({ state: 'provision_failed' }), null);
});

// --- The page's source contract ------------------------------------------

const PAGE = new URL('../routes/events/+page.svelte', import.meta.url);
const source = stripComments(Deno.readTextFileSync(PAGE));

/** A function's body as text: from its opener to the next top-level function. */
function body(name: string): string {
  const start = source.search(new RegExp(`\\n  (?:async )?function ${name}\\(`));
  assert(start >= 0, `the page defines ${name}`);
  const rest = source.slice(start + 1);
  const next = rest.slice(1).search(/\n  (?:async )?function \w+\(/);
  return next < 0 ? rest : rest.slice(0, next + 1);
}

/** The gate helpers ask the shared predicates afresh — never a stale paint. */
Deno.test('page — the gates ask the shared predicates at the press', () => {
  assert(/refusalOf\(eventCreatePossibility\(calendarsRead\)\)/.test(body('composeAllowed')));
  assert(/refusalOf\(calendarWritePossibility\(calendarsRead\)\)/.test(body('writeAllowed')));
});

/** Each door to the compose: the month double-click, the time-slot press and
 *  `new-event-btn` (whose handler is inline in the markup). */
Deno.test('page — every compose door asks the event-create predicate', () => {
  for (const door of ['openNewEventOnDay', 'openNewEventAt']) {
    assert(/if \(!composeAllowed\(\)\) return;/.test(body(door)), `${door} asks the predicate first`);
  }
  assert(
    /data-testid=\{IDS\.NEW_EVENT_BTN\}[\s\S]{0,120}onclick=\{\(\) => \{ if \(showNewEvent \|\| composeAllowed\(\)\) toggleNewEvent\(\); \}\}/
      .test(source),
    'new-event-btn opens the compose only through the predicate',
  );
});

Deno.test('page — the submit refuses through eventCreateTarget on the form', () => {
  const submit = body('handleCreateEvent');
  assert(/eventCreateTarget\(calendarsRead/.test(submit));
  assert(/formError = resolveLocalized\(target\.reason\)/.test(submit));
});

Deno.test('page — a failed save lands on the form, with the form kept open', () => {
  const submit = body('handleCreateEvent');
  const failure = submit.slice(submit.indexOf('catch'));
  assert(/formError =/.test(failure), 'the failure is said on the form');
  assert(/t\.events\.save_failed\(/.test(failure), 'in the save-failed sentence');
  assert(!/showNewEvent = false/.test(failure), 'the form stays open');
  assert(!/clearCompose\(\)/.test(failure), 'what was typed stays');
});

Deno.test('page — every other calendar write asks the calendar-write predicate', () => {
  assert(/if \(!writeAllowed\(\)\) return;/.test(body('handleCreateCalendar')), 'create-calendar');
  assert(
    /data-testid=\{IDS\.NEW_CALENDAR_BTN\}[\s\S]{0,200}if \(showNewCalendar \|\| writeAllowed\(\)\)/.test(source),
    'new-calendar-btn',
  );
  const importBody = body('handleImport');
  const ask = importBody.indexOf('writeAllowed()');
  const noFile = importBody.indexOf('t.events.ics_file_required');
  assert(ask >= 0, 'calendar-import-button asks the predicate');
  assert(noFile > ask, 'the no-file refusal stays, after the predicate');
});

Deno.test('page — a create re-listing with no calendar says the read\'s reason', () => {
  const create = body('handleCreateCalendar');
  const relist = create.indexOf('loadCalendars()');
  const ask = create.indexOf('refusalOf(calendarWritePossibility(calendarsRead))');
  assert(relist >= 0 && ask > relist, 'the re-listing is asked after it lands');
  assert(/sayWriteFailure\(e, t\.events\.error\.create_calendar\)/.test(create), 'a failure is mapped');
  assert(/sayWriteFailure\(e, t\.events\.error\.import\)/.test(body('handleImport')), 'an import failure is mapped');
});

Deno.test('page — the calendar load says unreadable calendars', () => {
  assert(/unreadableNotice\(read\)/.test(body('loadCalendars')));
});
