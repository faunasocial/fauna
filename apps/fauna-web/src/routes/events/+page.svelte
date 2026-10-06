<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { identity, reconnectTick } from '$lib/store';
  import { ensureWasm, attendeeDisplay, rsvpStatusLabel, reminderLabel, reminderPresets, resolveCalendarSelection, calendarIsDisplayed, workingDayStart, normalizeEventDatetimeInput, type ReminderPresetOption } from '$lib/wasm';
  import { resolveLocalized } from '$lib/i18n/localized';
  import {
    listCalendars,
    createCalendar,
    queryEvents,
    createEvent,
    deleteEvent,
    inviteToEvent,
    rsvpEvent,
    setReminder,
    removeReminder,
    importCalendar,
    exportCalendar,
  } from '$lib/api';
  import { onPushEvent, staleSurfacesForPushKind } from '$lib/rpc';
  import {
    loadEventDrafts,
    resumeEventDraft,
    scheduleEventDraftSave,
    clearEventDraft,
  } from '$lib/event-drafts';
  import type { Calendar, EventSummary, EventAttendee } from '$lib/api';
  import MiniCalendar from '$lib/components/MiniCalendar.svelte';
  import TimeGrid from '$lib/components/TimeGrid.svelte';
  import {
    pan,
    visibleDays,
    type CalendarViewMode,
    type PanDirection,
  } from '$lib/caltime';
  import MessageBanner from '$lib/components/MessageBanner.svelte';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // The Events page reads/writes the encrypted `bridge_caldav_*` store via the
  // `caldav*` wasm seam (events.md Decision B):
  // the same store + RPCs the mail-bridge MDA serves to Apple Calendar. Every
  // query row is the *full* flat VEVENT (attendees + reminder embedded), keyed by
  // the hex `uid_hash` (`EventSummary.id`) — the mutate target for
  // delete/rsvp/reminder/invite. The path is msek-gated: on a localhost/mail-off
  // nest the lists come back empty (the honest "enable mail" state, not an error).

  let ready = $state(false);
  let error = $state('');

  // View mode + visible range — events.md § Where logic lives.
  //
  // `viewMode` speaks the shared `caltime::CalendarViewMode::as_wire`
  // vocabulary, the same spelling as the `calendar-view-*` toggle ids. It used
  // to call the agenda view `'list'`, which no other app said and which
  // `from_wire` deliberately rejects.
  let viewMode = $state<CalendarViewMode>('agenda');
  // **The single anchor date**, as on linux (`CalendarViewState::selected_date`)
  // and tui (`focus`). Every view reads it: month renders its month, week its
  // week, day the day itself, agenda ignores it. `events-prev/next-month` pans
  // it by one visible range through the shared policy.
  //
  // This page previously carried a *second* anchor — a `viewedYear`/`viewedMonth`
  // pair that only the month grid read — so panning in week or day view moved
  // state nothing on screen could show. One anchor is what makes the pan
  // control mean the same thing in every mode.
  let selectedDate = $state(new Date());

  // The header label: the whole month in month view, otherwise the anchor day.
  let dateLabel = $derived(
    viewMode === 'month'
      ? selectedDate.toLocaleDateString(undefined, {
          year: 'numeric',
          month: 'long',
        })
      : selectedDate.toLocaleDateString(undefined, {
          weekday: 'short',
          year: 'numeric',
          month: 'short',
          day: 'numeric',
        }),
  );

  // One `events-prev-month` / `events-next-month` click — the shared
  // `caltime::pan` policy (one visible range per click, and nothing at all in
  // the date-unfiltered agenda). Wired unconditionally in every view mode:
  // the agenda arm is inert because `pan` returns the anchor unchanged, not
  // because the page branches on the mode — same as linux's `step_forward`.
  function panBy(direction: PanDirection) {
    selectedDate = pan(viewMode, selectedDate, direction);
  }

  // Calendar state
  let calendars = $state<Calendar[]>([]);
  let selectedCalendar = $state<Calendar | null>(null);
  let showNewCalendar = $state(false);
  let newCalendarName = $state('');
  let creatingCalendar = $state(false);
  // The `calendar-visibility` display filter's checked set (events.md § Where
  // logic lives → *Which calendars display*) — client-side only, never
  // persisted server-side and never a privacy/sharing control. An **empty**
  // set means "no filter" (the full union); `seedVisibleCalendars` fills it on
  // every calendar-list load so a fresh page paints every box checked rather
  // than leaning on that rule, mirroring tui/linux.
  let visibleCalendars = $state<Set<string>>(new Set());
  // The id of the most recently created calendar — creating a calendar does NOT
  // narrow the view to it (the union stays visible, matching the native apps),
  // but the next authoring action targets it so an event can be added immediately
  // with no extra "pick a calendar" step (events.md § Layout & flow).
  let lastCreatedCalendarId = $state<string | null>(null);
  // The calendar an authoring action (new event / import / export) targets when
  // no calendar is explicitly selected: the just-created one, else the first
  // available (events.md § Layout & flow — "target the just-created / first
  // available calendar when none is explicitly selected"; mirrors windows'
  // `SelectedCalendar ?? Calendars.FirstOrDefault()`).
  let activeCalendar = $derived(
    selectedCalendar
    ?? calendars.find((c) => c.id === lastCreatedCalendarId)
    ?? calendars[0]
    ?? null,
  );

  // Event list state
  let events = $state<EventSummary[]>([]);
  let loadingEvents = $state(false);
  let showNewEvent = $state(false);
  let creatingEvent = $state(false);

  // New event form
  let newEventSummary = $state('');
  let newEventDtstart = $state('');
  let newEventDtend = $state('');
  let newEventDescription = $state('');
  let newEventLocation = $state('');

  // Selected event detail (the full query row — no separate detail fetch)
  let selectedEvent = $state<EventSummary | null>(null);
  let selectedEventAttendees = $state<EventAttendee[]>([]);
  // event id → the calendar it was queried from, populated by every event query
  // (single-calendar or the no-selection union). In the union view a selected
  // event can belong to any owned calendar, so a detail-panel mutation targets
  // the event's OWN calendar via this map — web threads calendar_id at the
  // per-calendar query call site (native carries it on the FfiCalEvent row).
  let eventCalendars = $state<Record<string, string>>({});

  // Invite form: an email on the encrypted CalDAV path (caldav-server.md
  // § Scheduling). Cross-nest mailbox-less-Fauna delivery is fully automatic
  // — resolved from the typed CAL-ADDRESS alone (events.md § Scheduling).
  let inviteEmailInput = $state('');
  let inviting = $state(false);

  // Deleting
  let deleting = $state(false);

  // Reminder state
  let reminderOffset = $state<string | null>(null);
  let reminderLoading = $state(false);
  let selectedReminderPreset = $state('');

  // Import/Export
  let importing = $state(false);
  let importResult = $state<{ imported: number; skipped: number; total: number } | null>(null);
  let exporting = $state(false);

  // Server-push subscription (live RSVP refresh); unsubscribed on destroy.
  let unsubPush: (() => void) | null = null;
  let unsubReconnect: (() => void) | null = null;

  // The reminder preset catalog (value + localized label), populated in
  // onMount from the shared `reminder_presets` map (events.md § Reminders) —
  // the value is the cross-app `select(id, "PT1H")` contract.
  let reminderPresetOptions = $state<ReminderPresetOption[]>([]);

  /** The logged-in actor's email (`<handle>@<domain>`) — the VEVENT ORGANIZER /
   *  self RSVP CAL-ADDRESS on the encrypted CalDAV path. */
  function selfEmail(): string {
    const id = $identity;
    return id?.handle && id?.domain ? `${id.handle}@${id.domain}` : '';
  }

  /** Mail/CalDAV is off when `cfg.mail.msek` is None — the encrypted store then
   *  returns an empty calendar list (the lazy Personal provision needs the msek).
   *  With mail on, at least Personal is always present, so an empty list after a
   *  successful load is exactly the mail-off state (events.md § Persistence). */
  let mailDisabled = $derived(ready && !!$identity && calendars.length === 0);

  onMount(async () => {
    await ensureWasm();
    reminderPresetOptions = reminderPresets();
    identity.init();
    ready = true;
    // Draft-persistence v2, the `events` rail (events.md § Persistence): fetch +
    // unseal this actor's persisted event compose, so an in-progress event
    // survives a restart and reaches the user's other devices.
    //
    // ⚠ Deliberately NOT awaited. `loadEventDrafts` opens the WS client and does
    // a `fauna.drafts.get` round trip, and awaiting it here would put the whole
    // page behind that: `loadCalendars()` below is what renders the calendar
    // sidebar AND the New Event opener (`{#if calendars.length > 0}`), so a slow
    // or wedged connection would leave the page with no way to compose at all —
    // a draft convenience taking out the feature it decorates. The restore is
    // free to land whenever it lands, because `resumeDraftIfAny` is idempotent
    // and runs from BOTH doors: here when the load resolves, and again from the
    // New Event opener. The page never needs the draft to render.
    void loadEventDrafts().then(resumeDraftIfAny).catch(() => {
      // No identity yet / transport down: composing still works, unpersisted.
      // `loadEventDrafts` already logs the interesting half (a failed restore);
      // this arm only swallows the no-identity rejection.
    });
    await loadCalendars();
    // Render the initial no-selection union (every owned calendar's events) so
    // the Events page is populated before any calendar is picked (events.md
    // dated history 2026-07-15 — the converged no-selection union).
    await refreshEvents();

    // Live calendar refresh rides the one authenticated WS-RPC socket via the
    // shared push seam (`rpc.ts` § Server pushes) — `fauna.calendar.changed`,
    // fired by the nest after any durable write to one of this actor's
    // calendars (own other device, or an external MUA via the MDA; transport.md
    // § Push events, ratified 2026-07-17). The push is only a nudge: re-list
    // calendars (an externally-created calendar appears) and re-query the
    // selected one. Web has no quick-appearance poll, so this push is the
    // while-on-page mechanism here; the reconnect re-pull below covers pushes
    // lost across a socket gap. Checked
    // through the shared classifier (`transport.md` § Which surfaces a push
    // invalidates) rather than matching `kind` by hand — this also makes
    // `fauna.protocol.resync_required` refresh this page, which the old
    // exact-match check never did.
    unsubPush = onPushEvent(async (kind) => {
      if (!staleSurfacesForPushKind(kind).events) return;
      // Re-list calendars FIRST — an externally-created calendar must be in the
      // list before the no-selection union re-queries it — then re-query the
      // current scope. (Pre-union, refreshEvents only touched the selected
      // calendar, so list-staleness didn't matter; the union iterates the list.)
      await loadCalendars();
      await refreshEvents();
    });

    // Reconnect backstop: pushes fired while the socket was down are never
    // replayed, so a page that stayed mounted across the gap must re-pull —
    // this page had NO reconnect arm before adopting the shared classifier
    // (a real gap the seam's own audit is meant to catch, `transport.md` §
    // Which surfaces a push invalidates → Implementation status today).
    // `reconnectTick` is a `writable(0)` — it fires once on subscribe, so skip
    // that seed value (the feed page's idiom).
    let firstTick = true;
    unsubReconnect = reconnectTick.subscribe(() => {
      if (firstTick) { firstTick = false; return; }
      void loadCalendars().then(refreshEvents);
    });
  });

  onDestroy(() => {
    unsubPush?.();
    unsubReconnect?.();
  });

  async function loadCalendars() {
    const id = $identity;
    if (!id) return;
    error = '';
    try {
      const previous = calendars;
      calendars = await listCalendars(id.secretHex);
      seedVisibleCalendars(previous, calendars);
    } catch (e: any) {
      error = e.message || t.events.error.load_calendars;
    }
  }

  /** Fill `visibleCalendars` on a fresh calendar list so the page paints every
   *  box checked instead of leaning on the empty-set-is-union rule (mirrors
   *  tui/linux `seed_visible_calendars`). A **brand-new** calendar (not in
   *  `previous`) starts visible; an existing one keeps whatever the user
   *  chose. Unchecking every box empties the set, which the shared predicate
   *  reads as the union — so the next load re-seeds it. */
  function seedVisibleCalendars(previous: Calendar[], next: Calendar[]) {
    const seeded = new Set(visibleCalendars);
    for (const cal of next) {
      if (!previous.some((p) => p.id === cal.id)) seeded.add(cal.id);
    }
    if (seeded.size === 0) {
      for (const cal of next) seeded.add(cal.id);
    }
    visibleCalendars = seeded;
  }

  /** Flip one calendar's `calendar-visibility` checkbox — purely local display
   *  state, no refetch: `displayedEvents` re-filters the already-fetched
   *  `events` on the next paint. */
  function toggleCalendarVisibility(calendarId: string) {
    const next = new Set(visibleCalendars);
    if (!next.delete(calendarId)) next.add(calendarId);
    visibleCalendars = next;
  }

  // Monotonic query token. Each event load (refreshEvents / selectCalendar)
  // increments it and only commits its result if still current, so a slower
  // union query started earlier can't overwrite a fresher single-calendar
  // result that started later (the rapid create-calendar → select → create-event
  // sequence otherwise leaves the last-resolving stale query winning).
  let queryGen = 0;

  /** The events to display + each event's source calendar: when a calendar is
   *  selected, only its events; otherwise the union of every owned calendar's
   *  events (deduped by id). The encrypted store has no cross-calendar query, so
   *  the client fans out — mirrors windows' `EventsViewModel.QueryEventItemsAsync`
   *  and apple's `EventsVM.queryEventItems`, the richest existing pattern
   *  converged onto per priority #4 (web previously rendered nothing until a
   *  calendar was selected). The caller commits `events`/`eventCalendars` under
   *  the `queryGen` guard, so this is a pure fetch with no state mutation. */
  async function queryEventItems(
    secretHex: string,
  ): Promise<{ events: EventSummary[]; map: Record<string, string> }> {
    // Resolve the selection against the calendars that actually exist right
    // now (events.md § Where logic lives) — a selection naming a calendar
    // deleted here, or by a CalDAV MUA against the same store, must fall
    // back to the union rather than querying a gone id and reading empty
    // with no error (the stale-calendar-selection gap).
    const resolvedId = resolveCalendarSelection(
      selectedCalendar?.id,
      calendars.map((c) => c.id),
    );
    if (resolvedId) {
      const evs = await queryEvents(secretHex, resolvedId, selfEmail());
      const map: Record<string, string> = {};
      for (const ev of evs) map[ev.id] = resolvedId;
      return { events: evs, map };
    }
    const byId: Record<string, EventSummary> = {};
    const map: Record<string, string> = {};
    for (const cal of calendars) {
      for (const ev of await queryEvents(secretHex, cal.id, selfEmail())) {
        byId[ev.id] = ev;
        map[ev.id] = cal.id;
      }
    }
    return { events: Object.values(byId), map };
  }

  /** The calendar a detail-panel mutation (delete / rsvp / invite / reminder)
   *  targets: the event's own calendar (from the query map — correct in the
   *  union view where an event may not be from `selectedCalendar`), else the
   *  selected / first available calendar. */
  function calendarIdForEvent(ev: EventSummary | null): string | null {
    if (ev && eventCalendars[ev.id]) return eventCalendars[ev.id];
    return activeCalendar?.id ?? null;
  }

  /** The display color for an event: its own calendar's color (correct in the
   *  union view), falling back to the selected/active calendar. */
  function calendarColorForEvent(ev: EventSummary): string | undefined {
    const calId = eventCalendars[ev.id] ?? selectedCalendar?.id;
    return calendars.find((c) => c.id === calId)?.color || undefined;
  }

  /** Re-query the current scope (the selected calendar, or the no-selection
   *  union) and refresh the open detail panel from the matching row — attendees
   *  + reminder live on the row in the encrypted model, so a write that mutated
   *  the roster (rsvp / invite) reflects after a re-query, not a separate
   *  attendee fetch. */
  async function refreshEvents() {
    const id = $identity;
    if (!id) return;
    const gen = ++queryGen;
    try {
      const { events: evs, map } = await queryEventItems(id.secretHex);
      if (gen !== queryGen) return; // a newer load superseded this one
      events = evs;
      eventCalendars = map;
      if (selectedEvent) {
        const match = events.find((e) => e.id === selectedEvent!.id);
        if (match) {
          selectedEvent = match;
          selectedEventAttendees = match.attendees;
          reminderOffset = match.alarm || null;
        }
      }
    } catch (e: any) {
      error = e.message || t.events.error.load_events;
    }
  }

  async function handleCreateCalendar() {
    const id = $identity;
    if (!id || !newCalendarName.trim()) return;
    creatingCalendar = true;
    error = '';
    try {
      const { id: newCalendarId } = await createCalendar(id.secretHex, newCalendarName.trim());
      newCalendarName = '';
      showNewCalendar = false;
      await loadCalendars();
      // TARGET the just-created calendar for the next authoring action (so an
      // event can be added immediately with no extra "pick a calendar" step),
      // but do NOT narrow the view to it — the no-selection union stays visible,
      // matching the native apps, which "target the just-created / first
      // available calendar when none is explicitly selected" (events.md § Layout
      // & flow) rather than forcing a selection. (Web used to auto-SELECT here
      // only because the events list was gated on a selected calendar; the union
      // lift removed that gate, so the forced selection is no longer needed.)
      lastCreatedCalendarId = newCalendarId;
      await refreshEvents();
    } catch (e: any) {
      error = e.message || t.events.error.create_calendar;
    } finally {
      creatingCalendar = false;
    }
  }

  async function selectCalendar(cal: Calendar) {
    const id = $identity;
    if (!id) return;
    selectedCalendar = cal;
    selectedEvent = null;
    selectedEventAttendees = [];
    reminderOffset = null;
    loadingEvents = true;
    error = '';
    const gen = ++queryGen;
    try {
      const { events: evs, map } = await queryEventItems(id.secretHex);
      if (gen !== queryGen) return; // a newer load superseded this one
      events = evs;
      eventCalendars = map;
    } catch (e: any) {
      error = e.message || t.events.error.load_events;
      events = [];
    } finally {
      loadingEvents = false;
    }
  }

  // The local `YYYY-MM-DD` key for a date, from its LOCAL components (matching
  // MiniCalendar's `dateKey` + the `events-day-cell-{date}` testid).
  function localDateKey(d: Date): string {
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }

  // ── Draft persistence (the `events` rail) ─────────────────────────────────
  //
  // `events.md` § Persistence: the compose's five user-authored inputs rest in
  // `__drafts`, the New Event opener RESUMES them, and a day-cell gesture starts
  // fresh. Everything below is trigger glue — the encoding, the seal, the WS
  // calls and the launch gate all live in `$lib/event-drafts` over the shared
  // wasm face.

  /** Hand the compose's current five inputs to the debounced autosave. Called
   *  from every `event-form` input's `oninput`, which is this page's one door
   *  for compose text — the web twin of tui's `events::set_field`. Datetimes go
   *  RAW as typed (`events.md` § Persistence); the shared
   *  `normalizeEventDatetimeInput` rule stays at submit.
   *
   *  Reads the bound state rather than the event target, which is sound because
   *  `oninput` is DELEGATED in Svelte 5: it fires on the root during bubbling,
   *  after the direct listener `bind:value` installs on the element itself, so
   *  the `$state` is already current here. The programmatic openers call this
   *  themselves, since a prefill fires no input event. */
  function noteDraftEdit() {
    scheduleEventDraftSave({
      summary: newEventSummary,
      dtstart: newEventDtstart,
      dtend: newEventDtend,
      description: newEventDescription,
      location: newEventLocation,
    });
  }

  /** Paint a restored draft onto the compose buffers, unless the user has
   *  already authored something into them. Runs on mount and again when the New
   *  Event opener fires, since the launch load can land either side of the
   *  user's first click on a slow nest — the same late-restore rule the linux
   *  leg implements, so neither app's restore is latency-dependent. */
  function resumeDraftIfAny() {
    const draft = resumeEventDraft();
    if (!draft) return;
    // The caller-side safety: never paint over text the user has already
    // authored in THIS mount. `resumeEventDraft` itself is non-destructive, so
    // declining here simply leaves the draft available to the next open.
    if (newEventSummary || newEventDescription || newEventLocation) return;
    newEventSummary = draft.summary;
    newEventDescription = draft.description;
    newEventLocation = draft.location;
    // Keep whichever date prefill the opener supplied where the draft has none:
    // an empty datetime means the user never touched it.
    if (draft.dtstart) newEventDtstart = draft.dtstart;
    if (draft.dtend) newEventDtend = draft.dtend;
  }

  /** The New Event toggle. Opening RESUMES the persisted draft (clearing here is
   *  what would make the rail inert — it is the opener a user reaches for after
   *  relaunching to finish the event they were writing); closing is a plain
   *  cancel that leaves the draft alone, so it is still there next time. */
  function toggleNewEvent() {
    showNewEvent = !showNewEvent;
    if (showNewEvent) resumeDraftIfAny();
  }

  /** Empty the compose buffers AND the rail — a created event or a day-cell
   *  "start a new event here". Forgetting the rail half would leave a stale
   *  draft that reappears on the next launch for an event already created. */
  function clearCompose() {
    newEventSummary = '';
    newEventDtstart = '';
    newEventDtend = '';
    newEventDescription = '';
    newEventLocation = '';
    clearEventDraft();
  }

  // Double-click a month-grid day cell → open the new-event compose prefilled
  // with that date (Outlook day-cell model, events.md § Layout & flow, slice 2).
  // Prefills `dtstart` only (the `datetime-local` shape, at the shared working
  // start — `caltime::WORKING_DAY_START`); the user sets the end. Single-click
  // still drills into Day view (the grids' onSelectDate).
  function openNewEventOnDay(d: Date) {
    selectedDate = d;
    // This gesture means *start a new event here* (events.md § Layout & flow's
    // `create_event` bullet), so unlike the New Event opener it clears rather
    // than resuming — and the clear ticks the rail, so the abandoned draft stops
    // following the user to their other devices.
    clearCompose();
    const { hour, minute } = workingDayStart();
    const hh = String(hour).padStart(2, '0');
    const mm = String(minute).padStart(2, '0');
    newEventDtstart = `${localDateKey(d)}T${hh}:${mm}`;
    noteDraftEdit();
    showNewEvent = true;
  }

  async function handleCreateEvent() {
    const id = $identity;
    const cal = activeCalendar;
    if (!id || !cal || !newEventSummary.trim() || !newEventDtstart || !newEventDtend) return;
    creatingEvent = true;
    error = '';
    try {
      const uid = crypto.randomUUID();
      await createEvent(
        id.secretHex,
        {
          calendar_id: cal.id,
          uid,
          summary: newEventSummary.trim(),
          // The shared A2 input rule, as on every app: wall-clock, seconds-padded.
          // Never `toISOString()` — shifting to UTC made a midnight-to-midnight
          // event read back as a timed 22:00 block east of Greenwich, and put
          // every block off by the offset in the grid, which reads wall-clock.
          dtstart: normalizeEventDatetimeInput(newEventDtstart),
          dtend: normalizeEventDatetimeInput(newEventDtend),
          description: newEventDescription.trim() || undefined,
          location: newEventLocation.trim() || undefined,
        },
        selfEmail(),
      );
      // The event exists now, so it is no longer a draft: empty the buffers AND
      // tick the rail (events.md § Persistence). Deliberately here on the success
      // path — a create that threw leaves the user's text where they can still
      // see and retry it.
      clearCompose();
      showNewEvent = false;
      await refreshEvents();
    } catch (e: any) {
      error = e.message || t.events.error.create_event;
    } finally {
      creatingEvent = false;
    }
  }

  function selectEvent(ev: EventSummary) {
    // The encrypted query row is the full event — attendees + reminder ride on
    // it, so there is no separate detail/attendee/reminder fetch.
    selectedEvent = ev;
    selectedEventAttendees = ev.attendees;
    reminderOffset = ev.alarm || null;
    error = '';
  }

  async function handleDeleteEvent() {
    const id = $identity;
    const calId = calendarIdForEvent(selectedEvent);
    if (!id || !selectedEvent || !calId) return;
    deleting = true;
    error = '';
    try {
      await deleteEvent(id.secretHex, calId, selectedEvent.id);
      selectedEvent = null;
      selectedEventAttendees = [];
      reminderOffset = null;
      await refreshEvents();
    } catch (e: any) {
      error = e.message || t.events.error.delete_event;
    } finally {
      deleting = false;
    }
  }

  async function handleInvite() {
    const id = $identity;
    const calId = calendarIdForEvent(selectedEvent);
    if (!id || !selectedEvent || !calId || !inviteEmailInput.trim()) return;
    inviting = true;
    error = '';
    try {
      await inviteToEvent(
        id.secretHex,
        calId,
        selectedEvent.id,
        inviteEmailInput.trim(),
        selfEmail(),
      );
      inviteEmailInput = '';
      await refreshEvents();
    } catch (e: any) {
      error = e.message || t.events.error.invite;
    } finally {
      inviting = false;
    }
  }

  async function handleRsvp(response: string, ev?: EventSummary) {
    const id = $identity;
    const target = ev || selectedEvent;
    const calId = calendarIdForEvent(target);
    if (!id || !target || !calId) return;
    error = '';
    try {
      await rsvpEvent(id.secretHex, calId, target.id, response, selfEmail());
      await refreshEvents();
    } catch (e: any) {
      error = e.message || t.events.error.rsvp;
    }
  }

  async function handleSetReminder() {
    const id = $identity;
    const calId = calendarIdForEvent(selectedEvent);
    if (!id || !selectedEvent || !calId || !selectedReminderPreset) return;
    reminderLoading = true;
    error = '';
    try {
      await setReminder(id.secretHex, calId, selectedEvent.id, selectedReminderPreset);
      reminderOffset = selectedReminderPreset;
      selectedReminderPreset = '';
    } catch (e: any) {
      error = e.message || t.events.error.set_reminder;
    } finally {
      reminderLoading = false;
    }
  }

  async function handleRemoveReminder() {
    const id = $identity;
    const calId = calendarIdForEvent(selectedEvent);
    if (!id || !selectedEvent || !calId) return;
    reminderLoading = true;
    error = '';
    try {
      await removeReminder(id.secretHex, calId, selectedEvent.id);
      reminderOffset = null;
    } catch (e: any) {
      error = e.message || t.events.error.remove_reminder;
    } finally {
      reminderLoading = false;
    }
  }

  async function handleImport() {
    const id = $identity;
    const cal = activeCalendar;
    if (!id || !cal) return;
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = '.ics,text/calendar';
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      importing = true;
      importResult = null;
      error = '';
      try {
        const text = await file.text();
        importResult = await importCalendar(id.secretHex, cal.id, text, selfEmail());
        await refreshEvents();
      } catch (e: any) {
        error = e.message || t.events.error.import;
      } finally {
        importing = false;
      }
    };
    input.click();
  }

  async function handleExport() {
    const id = $identity;
    const cal = activeCalendar;
    if (!id || !cal) return;
    exporting = true;
    error = '';
    try {
      const icsText = await exportCalendar(id.secretHex, cal.id);
      const blob = new Blob([icsText], { type: 'text/calendar' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${cal.name || 'calendar'}.ics`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (e: any) {
      error = e.message || t.events.error.export;
    } finally {
      exporting = false;
    }
  }

  function isAuthor(): boolean {
    // `organized_by_me` is the canonical author-gate predicate computed in shared
    // Rust (`fauna_client_caldav::organized_by_me`) — the same one native exposes,
    // so web doesn't re-derive the organizer-email compare in TS (priority #2).
    return !!selectedEvent?.organized_by_me;
  }

  function myRsvpStatus(): string | null {
    const me = selfEmail();
    const att = selectedEventAttendees.find((a) => a.email === me);
    return att?.rsvp || null;
  }

  // The one shared status→color mapping (events.md § Attendee list presentation;
  // canonical apple `rsvpStatusColor` / android `rsvpStatusStyle`): going = green,
  // interested = yellow, declined = red, waitlisted = orange, invited / unknown
  // (incl. the verbatim `tentative`) = secondary.
  function statusColor(status: string): string {
    switch (status) {
      case 'going': return '#22c55e';
      case 'interested': return '#eab308';
      case 'declined': return '#ef4444';
      case 'waitlisted': return '#f97316';
      case 'invited': return '#9ca3af';
      default: return '#9ca3af';
    }
  }

  // The events the page shows right now — `events` filtered through the
  // shared `calendar_is_displayed` composition (events.md § Where logic lives
  // → *Which calendars display*): a live selection wins outright; with none,
  // `visibleCalendars` filters the union. Resolved ONCE per paint and handed
  // to every renderer below (agenda + `eventDates` + `gridEvents`) so they
  // cannot drift apart on the empty-set case — the bug tui/linux's own
  // per-view copies had before converging on one call. Purely local: toggling
  // a box re-filters the already-fetched `events`, no refetch.
  let displayedEvents = $derived.by(() => {
    const existingIds = calendars.map((c) => c.id);
    const visible = Array.from(visibleCalendars);
    return events.filter((ev) => {
      const calId = eventCalendars[ev.id];
      return !calId || calendarIsDisplayed(selectedCalendar?.id, existingIds, visible, calId);
    });
  });

  let eventDates = $derived.by(() => {
    const dates = new Set<string>();
    for (const ev of displayedEvents) {
      const d = new Date(ev.dtstart);
      dates.add(`${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`);
    }
    return dates;
  });

  // The events feeding the week/day timeline (TimeGrid) — TimeGrid classifies
  // all-day vs. timed itself via the shared `dayColumnLayout` (events.md
  // § Where logic lives), so `is_all_day` isn't threaded through here.
  let gridEvents = $derived.by(() => {
    return displayedEvents.map(ev => ({
      id: String(ev.id),
      summary: ev.summary,
      dtstart: ev.dtstart,
      dtend: ev.dtend,
      color: calendarColorForEvent(ev),
    }));
  });

  // The 7 Monday-start day-columns of the week containing the anchor — the
  // shared `caltime::visible_days` range itself, not a week-start snap this page
  // re-assembles with its own `addDays` loop (events.md § Where logic lives).
  let weekDays = $derived(visibleDays('week', selectedDate));

  // Empty-slot click in the week/day grid → open the new-event compose prefilled
  // to that date + time (the Outlook drill-in, mirroring the month grid's
  // empty-cell double-click). One-hour default duration.
  function openNewEventAt(d: Date, hour: number, minute: number) {
    selectedDate = d;
    // Same fresh-start gesture as the month grid's day cell, one view over.
    clearCompose();
    const hh = String(hour).padStart(2, '0');
    const mm = String(minute).padStart(2, '0');
    newEventDtstart = `${localDateKey(d)}T${hh}:${mm}`;
    newEventDtend = `${localDateKey(d)}T${String(Math.min(hour + 1, 23)).padStart(2, '0')}:${mm}`;
    noteDraftEdit();
    showNewEvent = true;
  }
</script>

<h1 data-testid={IDS.PAGE_HEADING}>{t.events.title}</h1>

<div class="view-toggle" data-testid={IDS.EVENTS_VIEW_TOGGLE}>
  <button class="view-btn" data-testid={IDS.CALENDAR_VIEW_AGENDA} class:active={viewMode === 'agenda'} onclick={() => { viewMode = 'agenda'; }}>{t.events.view_agenda}</button>
  <button class="view-btn" data-testid={IDS.CALENDAR_VIEW_MONTH} class:active={viewMode === 'month'} onclick={() => { viewMode = 'month'; }}>{t.events.view_month}</button>
  <button class="view-btn" data-testid={IDS.CALENDAR_VIEW_WEEK} class:active={viewMode === 'week'} onclick={() => { viewMode = 'week'; }}>{t.events.view_week}</button>
  <button class="view-btn" data-testid={IDS.CALENDAR_VIEW_DAY} class:active={viewMode === 'day'} onclick={() => { viewMode = 'day'; }}>{t.events.view_day}</button>
  <!-- The pan control is page-level and present in EVERY view mode: it moves the
       page's one anchor by one visible range. It used to live inside the month
       grid, so it existed in the DOM only in month view — which is why panning
       "did nothing" in week and day. Agenda's inertness comes from the shared
       `caltime::pan` policy, not from hiding or disabling the buttons. -->
  <span class="pan-nav">
    <button class="view-btn pan-btn" data-testid={IDS.EVENTS_PREV_MONTH} onclick={() => panBy('backward')} aria-label={t.common.previous}>&lsaquo;</button>
    <button class="view-btn pan-btn" data-testid={IDS.EVENTS_NEXT_MONTH} onclick={() => panBy('forward')} aria-label={t.common.next}>&rsaquo;</button>
  </span>
  <span data-testid={IDS.CALENDAR_DATE_LABEL} class="calendar-date-label">{dateLabel}</span>
</div>

<MessageBanner bind:error />

{#if !ready}
  <p class="muted">{t.common.loading}</p>
{:else if !$identity}
  <p class="muted">{t.common.identity_required}</p>
{:else}
  <div class="events-layout">
    <!-- Left panel -->
    <div class="events-sidebar">
      <section class="calendar-section">
        <div class="section-header">
          <h2>{t.events.calendars}</h2>
          <button
            data-testid={IDS.NEW_CALENDAR_BTN}
            class="btn"
            onclick={() => { showNewCalendar = !showNewCalendar; }}
          >
            {showNewCalendar ? t.common.cancel : t.events.new_calendar}
          </button>
        </div>

        {#if showNewCalendar}
          <div class="inline-form">
            <input
              data-testid={IDS.CALENDAR_NAME}
              type="text"
              placeholder={t.events.calendar_name}
              bind:value={newCalendarName}
              class="input"
            />
            <button
              data-testid={IDS.CREATE_CALENDAR}
              class="btn primary"
              onclick={handleCreateCalendar}
              disabled={creatingCalendar || !newCalendarName.trim()}
            >
              {creatingCalendar ? t.common.creating : t.common.create}
            </button>
          </div>
        {/if}

        {#if calendars.length === 0}
          {#if mailDisabled}
            <p class="muted enable-mail-state">
              {t.events.mail_required}
            </p>
          {:else}
            <p class="muted">{t.events.no_calendars}</p>
          {/if}
        {:else}
          {#each calendars as cal}
            <div class="calendar-row">
              <button
                data-testid={IDS.CALENDAR_ITEM}
                class="calendar-item"
                class:active={selectedCalendar?.id === cal.id}
                onclick={() => selectCalendar(cal)}
              >
                <span class="calendar-color" style="background: {cal.color || 'var(--accent)'}"></span>
                <span class="calendar-name">{cal.name}</span>
              </button>
              <input
                type="checkbox"
                data-testid={IDS.CALENDAR_VISIBILITY}
                class="calendar-visibility-toggle"
                checked={visibleCalendars.has(cal.id)}
                onchange={() => toggleCalendarVisibility(cal.id)}
              />
            </div>
          {/each}
        {/if}
      </section>

      {#if calendars.length > 0}
        <section class="mini-cal-section">
          <MiniCalendar
            {selectedDate}
            onSelectDate={(d) => { selectedDate = d; }}
            {eventDates}
          />
        </section>
      {/if}

      {#if calendars.length > 0}
        <section class="events-list-section">
          <div class="section-header">
            <h2>{t.events.title}</h2>
            <div class="header-actions">
              <button
                data-testid={IDS.NEW_EVENT_BTN}
                class="btn"
                onclick={toggleNewEvent}
              >
                {showNewEvent ? t.common.cancel : t.events.new_event}
              </button>
              <button
                class="btn"
                onclick={handleImport}
                disabled={importing}
              >
                {importing ? t.events.importing : t.events.import_ics}
              </button>
              <button
                class="btn"
                onclick={handleExport}
                disabled={exporting}
              >
                {exporting ? t.events.exporting : t.events.export_ics}
              </button>
            </div>
          </div>

          {#if importResult}
            <div class="import-result">
              {t.events.import_result({ imported: String(importResult.imported), skipped: String(importResult.skipped), total: String(importResult.total) })}
              <button class="btn" onclick={() => { importResult = null; }}>{t.common.dismiss}</button>
            </div>
          {/if}

          {#if showNewEvent}
            <div class="inline-form">
              <label class="form-label">
                <input
                  data-testid={IDS.EVENT_SUMMARY}
                  type="text"
                  placeholder={t.events.summary}
                  bind:value={newEventSummary}
                  oninput={noteDraftEdit}
                  class="input"
                  required
                />
              </label>
              <label class="form-label">
                {t.events.start}
                <input
                  data-testid={IDS.EVENT_DTSTART}
                  type="datetime-local"
                  bind:value={newEventDtstart}
                  oninput={noteDraftEdit}
                  class="input"
                  required
                />
              </label>
              <label class="form-label">
                {t.events.end}
                <input
                  data-testid={IDS.EVENT_DTEND}
                  type="datetime-local"
                  bind:value={newEventDtend}
                  oninput={noteDraftEdit}
                  class="input"
                  required
                />
              </label>
              <textarea
                data-testid={IDS.EVENT_FORM_DESCRIPTION}
                placeholder={t.events.description}
                bind:value={newEventDescription}
                oninput={noteDraftEdit}
                class="input textarea"
              ></textarea>
              <input
                data-testid={IDS.EVENT_FORM_LOCATION}
                type="text"
                placeholder={t.events.location}
                bind:value={newEventLocation}
                oninput={noteDraftEdit}
                class="input"
              />
              <button
                data-testid={IDS.CREATE_EVENT}
                class="btn primary"
                onclick={handleCreateEvent}
                disabled={creatingEvent || !newEventSummary.trim() || !newEventDtstart || !newEventDtend}
              >
                {creatingEvent ? t.common.creating : t.events.create_event}
              </button>
            </div>
          {/if}

          {#if loadingEvents}
            <p class="muted">{t.events.loading_events}</p>
          {:else if viewMode === 'day'}
            <div data-testid={IDS.CALENDAR_DAY_TIMELINE}>
              <TimeGrid
                dates={[selectedDate]}
                events={gridEvents}
                onSelectEvent={(id) => {
                  const ev = events.find(e => String(e.id) === id);
                  if (ev) selectEvent(ev);
                }}
                onEmptySlot={openNewEventAt}
              />
            </div>
          {:else if viewMode === 'week'}
            <div data-testid={IDS.CALENDAR_WEEK_GRID}>
              <TimeGrid
                dates={weekDays}
                events={gridEvents}
                onSelectEvent={(id) => {
                  const ev = events.find(e => String(e.id) === id);
                  if (ev) selectEvent(ev);
                }}
                onEmptySlot={openNewEventAt}
              />
            </div>
          {:else if viewMode === 'month'}
            <div class="month-grid-view">
              <!-- Controlled by the page anchor: the month shown is the anchor's
                   month, and `events-prev/next-month` in the header pans it. -->
              <MiniCalendar
                primary
                {selectedDate}
                viewYear={selectedDate.getFullYear()}
                viewMonth={selectedDate.getMonth()}
                onSelectDate={(d) => { selectedDate = d; viewMode = 'day'; }}
                onSelectDateDouble={(d) => openNewEventOnDay(d)}
                {eventDates}
              />
            </div>
          {:else if displayedEvents.length === 0}
            <p class="muted">{t.events.no_events_in_calendar}</p>
          {:else}
            {#each displayedEvents as ev}
              <div
                data-testid={IDS.EVENT_CARD}
                class="event-card"
                class:active={selectedEvent?.id === ev.id}
                style="border-left: 3px solid {calendarColorForEvent(ev) || 'var(--accent)'}"
                role="button"
                tabindex="0"
                onclick={(e) => { if (!(e.target as HTMLElement).closest('.rsvp-inline')) selectEvent(ev); }}
                onkeydown={(e) => { if (e.key === 'Enter' && !(e.target as HTMLElement).closest('.rsvp-inline')) selectEvent(ev); }}
              >
                <div data-testid={IDS.EVENT_CARD_SUMMARY} class="event-card-summary">{ev.summary}</div>
                <div class="event-card-dates">
                  {new Date(ev.dtstart).toLocaleString()} &ndash; {new Date(ev.dtend).toLocaleString()}
                </div>
                {#if ev.location}
                  <div class="event-card-location">{ev.location}</div>
                {/if}
                <div class="rsvp-inline">
                  <button data-testid={IDS.EVENT_RSVP_GOING} class="btn-rsvp" onclick={() => handleRsvp('going', ev)}>{t.events.rsvp.going}</button>
                  <button data-testid={IDS.EVENT_RSVP_INTERESTED} class="btn-rsvp" onclick={() => handleRsvp('interested', ev)}>{t.events.rsvp.interested}</button>
                  <button data-testid={IDS.EVENT_RSVP_DECLINE} class="btn-rsvp" onclick={() => handleRsvp('declined', ev)}>{t.events.rsvp.decline}</button>
                </div>
              </div>
            {/each}
          {/if}
        </section>
      {/if}
    </div>

    <!-- Right panel -->
    <div class="event-detail">
      {#if !selectedEvent}
        <p class="muted">{t.events.select_calendar_event_hint}</p>
      {:else}
        <h2 data-testid={IDS.EVENT_DETAIL_SUMMARY}>{selectedEvent.summary}</h2>
        <div class="detail-meta">
          <p data-testid={IDS.EVENT_DETAIL_TIME} class="detail-dates">
            {new Date(selectedEvent.dtstart).toLocaleString()} &ndash; {new Date(selectedEvent.dtend).toLocaleString()}
          </p>
          {#if selectedEvent.location}
            <p data-testid={IDS.EVENT_DETAIL_LOCATION} class="detail-location">{t.events.location}: {selectedEvent.location}</p>
          {/if}
          {#if selectedEvent.description}
            <p data-testid={IDS.EVENT_DETAIL_DESCRIPTION} class="detail-description">{selectedEvent.description}</p>
          {/if}
        </div>

        <!-- Attendees (embedded on the encrypted event row) -->
        <section class="attendees-section">
          <h3>{t.events.attendees_count({ count: String(selectedEventAttendees.length) })}</h3>
          {#if selectedEventAttendees.length === 0}
            <p class="muted">{t.events.no_attendees}</p>
          {:else}
            <div class="attendees-list" data-testid={IDS.ATTENDEE_LIST}>
              {#each selectedEventAttendees as att}
                {@const attView = attendeeDisplay(att.name, att.email)}
                <div data-testid={IDS.ATTENDEE_ITEM} class="attendee-item">
                  <span class="attendee-monogram" aria-hidden="true">{attView.monogram}</span>
                  <div class="attendee-info">
                    <span data-testid={IDS.ATTENDEE_ID} class="attendee-name" title={att.email}>{attView.display_name}</span>
                    {#if attView.secondary_email}
                      <span class="attendee-email">{attView.secondary_email}</span>
                    {/if}
                  </div>
                  <span
                    data-testid={IDS.ATTENDEE_STATUS}
                    class="status-capsule"
                    style="color: {statusColor(att.rsvp)}; background: {statusColor(att.rsvp)}33;"
                  >
                    {resolveLocalized(rsvpStatusLabel(att.rsvp))}
                  </span>
                </div>
              {/each}
            </div>
          {/if}
        </section>

        {#if isAuthor()}
          <!-- Author actions: invite + delete -->
          <section class="invite-section">
            <h3>{t.events.invite.button}</h3>
            <div class="invite-form">
              <input
                data-testid={IDS.ATTENDEE_INVITE_FIELD}
                type="email"
                placeholder={t.events.invite.email_placeholder}
                bind:value={inviteEmailInput}
                class="input"
              />
              <button
                data-testid={IDS.ATTENDEE_INVITE_BUTTON}
                class="btn primary"
                onclick={handleInvite}
                disabled={inviting || !inviteEmailInput.trim()}
              >
                {inviting ? t.events.invite.inviting : t.events.invite.button}
              </button>
            </div>
          </section>

          <section class="delete-section">
            <button
              data-testid={IDS.EVENT_DELETE_BTN}
              class="btn danger-btn"
              onclick={handleDeleteEvent}
              disabled={deleting}
            >
              {deleting ? t.events.deleting : t.events.delete_event}
            </button>
          </section>
        {/if}

        <!-- RSVP — available to everyone, including the host (who can set their
             own attendance); the inline event-card RSVP buttons already offer
             this to all, so the detail panel must too (no author gate). -->
        <section class="rsvp-section">
          <h3>{t.events.rsvp.title}</h3>
          {#if myRsvpStatus()}
            <p class="current-rsvp">
              {t.events.your_status}
              <span
                class="status-badge"
                style="background: {statusColor(myRsvpStatus()!)}; color: #fff;"
              >{resolveLocalized(rsvpStatusLabel(myRsvpStatus()!))}</span>
            </p>
          {/if}
          <div class="rsvp-buttons">
            <button data-testid={IDS.EVENT_DETAIL_RSVP_GOING} class="btn rsvp-going" onclick={() => handleRsvp('going')}>{t.events.rsvp.going}</button>
            <button data-testid={IDS.EVENT_DETAIL_RSVP_INTERESTED} class="btn rsvp-interested" onclick={() => handleRsvp('interested')}>{t.events.rsvp.interested}</button>
            <button data-testid={IDS.EVENT_DETAIL_RSVP_DECLINE} class="btn rsvp-decline" onclick={() => handleRsvp('declined')}>{t.events.rsvp.decline}</button>
          </div>
        </section>

        <!-- Reminder -->
        <section class="reminder-section">
          <h3>{t.events.reminder.title}</h3>
          {#if reminderOffset}
            <p class="current-reminder">
              {t.events.reminder.current} <strong data-testid={IDS.EVENT_DETAIL_REMINDER_CURRENT}>{resolveLocalized(reminderLabel(reminderOffset))}</strong>
              <button
                data-testid={IDS.EVENT_DETAIL_REMINDER_REMOVE}
                class="btn remove-reminder-btn"
                onclick={handleRemoveReminder}
                disabled={reminderLoading}
              >{t.common.remove}</button>
            </p>
          {:else}
            <div class="reminder-form">
              <select data-testid={IDS.EVENT_DETAIL_REMINDER_SELECT} bind:value={selectedReminderPreset} class="input reminder-select">
                <option value="">{t.events.reminder.select_placeholder}</option>
                {#each reminderPresetOptions as preset}
                  <option value={preset.value}>{resolveLocalized(preset.label)}</option>
                {/each}
              </select>
              <button
                data-testid={IDS.EVENT_DETAIL_REMINDER_SET}
                class="btn primary"
                onclick={handleSetReminder}
                disabled={reminderLoading || !selectedReminderPreset}
              >
                {reminderLoading ? t.events.setting : t.events.set_reminder}
              </button>
            </div>
          {/if}
        </section>
      {/if}
    </div>
  </div>
{/if}

<style>
  .events-layout {
    display: flex;
    gap: 1.5rem;
    height: calc(100vh - 5rem);
  }
  .events-sidebar {
    width: 340px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 1rem;
    overflow-y: auto;
  }
  .event-detail {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  .section-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.5rem;
  }
  .section-header h2 {
    margin: 0;
    font-size: 1rem;
  }
  .inline-form {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
  }
  .form-label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .input {
    width: 100%;
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--text);
    font-size: 0.875rem;
    box-sizing: border-box;
  }
  .textarea {
    min-height: 60px;
    resize: vertical;
    font-family: inherit;
  }
  .btn {
    padding: 0.5rem 1rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
    font-size: 0.875rem;
    white-space: nowrap;
  }
  .btn:hover { background: var(--bg-hover); }
  .btn.primary { background: var(--accent); color: #fff; border-color: var(--accent); }
  .btn.primary:hover { background: var(--accent-hover); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .danger-btn {
    background: transparent;
    color: var(--danger);
    border-color: var(--danger);
  }
  .danger-btn:hover { background: var(--danger); color: #fff; }

  /* Calendar items */
  .calendar-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.375rem;
  }
  .calendar-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex: 1;
    min-width: 0;
    padding: 0.625rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
    font-size: 0.875rem;
    text-align: left;
  }
  .calendar-item:hover { background: var(--bg-hover); }
  .calendar-item.active { border-color: var(--accent); background: var(--bg-hover); }
  .calendar-color {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .calendar-name { font-weight: 600; flex: 1; }
  .calendar-visibility-toggle { flex-shrink: 0; cursor: pointer; }
  .enable-mail-state { line-height: 1.4; }

  /* Event cards */
  .event-card {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    width: 100%;
    padding: 0.625rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
    margin-bottom: 0.375rem;
    text-align: left;
  }
  .event-card:hover { background: var(--bg-hover); }
  .event-card.active { border-color: var(--accent); background: var(--bg-hover); }
  .event-card-summary { font-weight: 600; font-size: 0.875rem; }
  .event-card-dates { font-size: 0.75rem; color: var(--text-muted); }
  .event-card-location { font-size: 0.75rem; color: var(--text-muted); }
  .rsvp-inline {
    display: flex;
    gap: 0.375rem;
    margin-top: 0.375rem;
  }
  .btn-rsvp {
    font-size: 0.7rem;
    padding: 0.15rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg-surface);
    color: var(--text);
    cursor: pointer;
  }
  .btn-rsvp:hover { background: var(--bg-hover); }

  /* Detail panel */
  .detail-meta {
    font-size: 0.875rem;
    color: var(--text-muted);
  }
  .detail-dates { margin-bottom: 0.25rem; }
  .detail-location { margin-bottom: 0.25rem; }
  .detail-description {
    white-space: pre-wrap;
    margin-top: 0.5rem;
    color: var(--text);
  }

  /* Attendees */
  .attendees-section h3 {
    margin-bottom: 0.5rem;
    font-size: 0.95rem;
  }
  /* Attendee list — one enriched row per attendee (events.md § Attendee list
     presentation): monogram avatar + name + email-beneath + colored RSVP capsule.
     A vertical list (not wrapping chips) scales to a long roster. */
  .attendees-list {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-bottom: 0.5rem;
  }
  .attendee-item {
    display: flex;
    align-items: center;
    gap: 0.625rem;
    padding: 0.375rem 0.25rem;
  }
  .attendee-monogram {
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: var(--accent);
    color: #fff;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.8rem;
    font-weight: 600;
  }
  .attendee-info {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1 1 auto;
  }
  .attendee-name {
    font-size: 0.9rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .attendee-email {
    font-size: 0.75rem;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status-capsule {
    flex: 0 0 auto;
    font-size: 0.7rem;
    font-weight: 600;
    padding: 0.125rem 0.5rem;
    border-radius: 999px;
  }

  /* Invite form */
  .invite-section h3,
  .rsvp-section h3 {
    margin-bottom: 0.5rem;
    font-size: 0.95rem;
  }
  .invite-form {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    max-width: 400px;
    margin-bottom: 1rem;
  }
  .invite-form .input { flex: 1; }

  /* RSVP */
  .rsvp-buttons {
    display: flex;
    gap: 0.5rem;
  }
  .rsvp-going { background: #22c55e; color: #fff; border-color: #22c55e; }
  .rsvp-going:hover { background: #16a34a; }
  .rsvp-interested { background: #eab308; color: #fff; border-color: #eab308; }
  .rsvp-interested:hover { background: #ca8a04; }
  .rsvp-decline { background: #ef4444; color: #fff; border-color: #ef4444; }
  .rsvp-decline:hover { background: #dc2626; }

  /* Delete section */
  .delete-section {
    margin-top: 1rem;
    padding-top: 1rem;
    border-top: 1px solid var(--border);
  }

  /* Current RSVP display */
  .current-rsvp {
    font-size: 0.875rem;
    margin-bottom: 0.5rem;
  }

  /* Reminder section */
  .reminder-section {
    margin-top: 1rem;
    padding-top: 1rem;
    border-top: 1px solid var(--border);
  }
  .reminder-section h3 {
    margin-bottom: 0.5rem;
    font-size: 0.95rem;
  }
  .reminder-form {
    display: flex;
    gap: 0.5rem;
    align-items: center;
    max-width: 400px;
  }
  .reminder-select {
    max-width: 200px;
  }
  .current-reminder {
    font-size: 0.875rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .remove-reminder-btn {
    font-size: 0.75rem;
    padding: 0.25rem 0.5rem;
    color: var(--danger);
    border-color: var(--danger);
  }
  .remove-reminder-btn:hover {
    background: var(--danger);
    color: #fff;
  }

  /* Import/Export */
  .header-actions {
    display: flex;
    gap: 0.375rem;
    align-items: center;
    flex-wrap: wrap;
  }
  .import-result {
    padding: 0.625rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-surface);
    font-size: 0.8rem;
    margin-bottom: 0.5rem;
  }

  /* Mini calendar */
  .mini-cal-section { margin-top: 1rem; }

  /* View toggle */
  .view-toggle { display: flex; gap: 0.25rem; margin-right: 0.5rem; }
  .view-btn {
    padding: 0.2rem 0.5rem; border: 1px solid var(--border); border-radius: 4px;
    background: var(--bg-surface); color: var(--text-muted); font-size: 0.75rem;
    cursor: pointer;
  }
  .view-btn:hover { background: var(--bg-hover); }
  .view-btn.active { background: var(--accent); color: #fff; border-color: var(--accent); }
  /* The pan control sits just after the mode toggle, separated from it — it acts
     on the anchor, not on which view is showing. */
  .pan-nav { display: flex; gap: 0.25rem; margin-left: 0.5rem; }
  .pan-btn { line-height: 1; font-size: 0.9rem; padding: 0.2rem 0.45rem; }
  .calendar-date-label { margin-left: auto; font-size: 0.8rem; color: var(--text-muted); font-weight: 500; }

  .muted { color: var(--text-muted); }

  @media (max-width: 768px) {
    .events-layout { flex-direction: column; height: auto; }
    .events-sidebar { width: 100%; }
  }
</style>
