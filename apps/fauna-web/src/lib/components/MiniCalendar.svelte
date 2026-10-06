<script lang="ts">
  import { untrack } from 'svelte';
  import {
    monthGrid,
    prevMonth as caltimePrevMonth,
    nextMonth as caltimeNextMonth,
    localeWeekStart,
    weekdayCells,
  } from '$lib/caltime';
  import { t } from '$lib/i18n/strings';

  interface Props {
    selectedDate: Date;
    onSelectDate: (date: Date) => void;
    // Optional: fired on a *double*-click of a day cell (the Outlook day-cell
    // model — single-click drills into Day view, double-click opens the new-event
    // compose, events.md § Layout & flow). Only the page-level month grids wire
    // it; when set, the single-click is debounced so the first click of a double
    // doesn't drill in (and unmount the grid) before `dblclick` fires.
    onSelectDateDouble?: (date: Date) => void;
    eventDates?: Set<string>;
    // `primary` marks the page-level month-grid instance: it owns the
    // page-scoped testid `events-month-grid` (and the indexed day cells). The
    // sidebar mini-calendar (primary=false) must NOT emit them — otherwise the
    // ids duplicate (sidebar + main month view) and leak onto non-month views.
    primary?: boolean;
    // **Controlled mode.** When given, the grid shows this month and hides its
    // own header nav: the caller owns the visible month. The Events page passes
    // the month of its single anchor date, because `events-prev/next-month` is
    // page-level — it pans the anchor by one *visible range*, which is a month
    // only in month view (events.md § Where logic lives → View mode + visible
    // range). Left off, the grid keeps its own month and nav — the standalone
    // date-browser the sidebar wants.
    viewYear?: number;
    viewMonth?: number;
  }

  let {
    selectedDate,
    onSelectDate,
    onSelectDateDouble,
    eventDates = new Set(),
    primary = false,
    viewYear: controlledYear,
    viewMonth: controlledMonth,
  }: Props = $props();

  const controlled = $derived(controlledYear !== undefined && controlledMonth !== undefined);

  // Single-vs-double-click disambiguation for the day cells. Only debounce when a
  // double-click handler is wired (the primary month grids): a bare single-click
  // grid (the sidebar mini-cal) stays immediate.
  let clickTimer: ReturnType<typeof setTimeout> | null = null;

  function handleDayClick(d: Date) {
    if (!onSelectDateDouble) {
      onSelectDate(d);
      return;
    }
    if (clickTimer) clearTimeout(clickTimer);
    clickTimer = setTimeout(() => {
      clickTimer = null;
      onSelectDate(d);
    }, 220);
  }

  function handleDayDoubleClick(d: Date) {
    if (clickTimer) {
      clearTimeout(clickTimer);
      clickTimer = null;
    }
    onSelectDateDouble?.(d);
  }

  // Uncontrolled (sidebar) mode: initialise the visible month from the selected
  // date once, then let this component's own prev/next nav move it. In
  // controlled mode these are ignored in favour of the caller's props below.
  let ownMonth = $state(untrack(() => selectedDate.getMonth()));
  let ownYear = $state(untrack(() => selectedDate.getFullYear()));

  const viewMonth = $derived(controlled ? controlledMonth! : ownMonth);
  const viewYear = $derived(controlled ? controlledYear! : ownYear);

  // Single-letter weekday initials for this compact header. The generated i18n
  // catalog only has 3-letter (`time.weekday_mon`) and full (`time.weekday_full_mon`)
  // forms — no narrow single-letter key — so this stays on the OS's `Intl`
  // narrow-width formatter rather than widening the header to 3 letters (a
  // layout change, not an i18n fix). 2024-01-01 is a known Monday, matching the
  // Monday-start grid below; this was previously a hardcoded English array (the
  // same bug windows/android/linux/tui had, fixed there via the i18n catalog).
  // ONE locale value feeds this header AND the grid below it. Probing twice
  // is the linux bug: a header that names a different week than its columns.
  const weekStartDay = localeWeekStart();
  const DOW = weekdayCells(weekStartDay, (d) =>
    d.toLocaleDateString(undefined, { weekday: 'narrow' }),
  );

  // The 42-cell month grid rides the shared `fauna_core::caltime::month_grid`
  // (via `$lib/caltime`) instead of re-deriving the prev/next-month padding in
  // JS `Date` arithmetic — priority #2/#4, events.md § Where logic lives. The
  // calendar starts on the locale's first day (the `DOW` header is rotated to
  // match, off the same value); `inMonth` marks the padding days.
  let cells = $derived.by(() =>
    monthGrid(viewYear, viewMonth, weekStartDay).map((c) => ({
      day: c.date.getDate(),
      date: c.date,
      inMonth: c.inMonth,
    })),
  );

  function dateKey(d: Date): string {
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }

  function isToday(d: Date): boolean {
    const now = new Date();
    return d.getFullYear() === now.getFullYear() && d.getMonth() === now.getMonth() && d.getDate() === now.getDate();
  }

  function isSelected(d: Date): boolean {
    return d.getFullYear() === selectedDate.getFullYear() && d.getMonth() === selectedDate.getMonth() && d.getDate() === selectedDate.getDate();
  }

  function prevMonth() {
    const prev = caltimePrevMonth(ownYear, ownMonth);
    ownYear = prev.year;
    ownMonth = prev.month0;
  }

  function nextMonth() {
    const next = caltimeNextMonth(ownYear, ownMonth);
    ownYear = next.year;
    ownMonth = next.month0;
  }

  // Resolved through the generated i18n catalog (unlike DOW above, a full month
  // name has no narrow-width concern, so this tracks the app's own translation
  // — matching linux's identical fix — rather than the OS's locale).
  const MONTH_NAMES = [
    t.time.month_full_jan,
    t.time.month_full_feb,
    t.time.month_full_mar,
    t.time.month_full_apr,
    t.time.month_full_may,
    t.time.month_full_jun,
    t.time.month_full_jul,
    t.time.month_full_aug,
    t.time.month_full_sep,
    t.time.month_full_oct,
    t.time.month_full_nov,
    t.time.month_full_dec,
  ];
</script>

<div class="mini-cal">
  <div class="mini-cal-header" class:centered={controlled}>
    {#if !controlled}
      <button class="mini-cal-nav" onclick={prevMonth}>&lsaquo;</button>
    {/if}
    <span class="mini-cal-title">{MONTH_NAMES[viewMonth]} {viewYear}</span>
    {#if !controlled}
      <button class="mini-cal-nav" onclick={nextMonth}>&rsaquo;</button>
    {/if}
  </div>
  <div class="mini-cal-dow">
    {#each DOW as d}
      <span>{d}</span>
    {/each}
  </div>
  <div class="mini-cal-grid" data-testid={primary ? 'events-month-grid' : undefined}>
    {#each cells as cell}
      <button
        class="mini-cal-day"
        data-testid={primary ? `events-day-cell-${dateKey(cell.date)}` : undefined}
        class:out-of-month={!cell.inMonth}
        class:today={isToday(cell.date)}
        class:selected={isSelected(cell.date)}
        class:has-event={eventDates.has(dateKey(cell.date))}
        onclick={() => handleDayClick(cell.date)}
        ondblclick={() => handleDayDoubleClick(cell.date)}
      >
        {cell.day}
      </button>
    {/each}
  </div>
</div>

<style>
  .mini-cal { font-size: 0.8rem; }
  .mini-cal-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 0.25rem; }
  /* Controlled mode has no nav buttons to sit either side of the title — the
     page header owns `events-prev/next-month` — so centre it instead. */
  .mini-cal-header.centered { justify-content: center; }
  .mini-cal-title { font-weight: 600; font-size: 0.85rem; }
  .mini-cal-nav { background: none; border: none; cursor: pointer; font-size: 1.1rem; color: var(--text-muted); padding: 0 0.25rem; }
  .mini-cal-nav:hover { color: var(--text); }
  .mini-cal-dow { display: grid; grid-template-columns: repeat(7, 1fr); text-align: center; color: var(--text-muted); font-size: 0.7rem; font-weight: 600; margin-bottom: 0.125rem; }
  .mini-cal-grid { display: grid; grid-template-columns: repeat(7, 1fr); gap: 1px; }
  .mini-cal-day {
    background: none; border: none; cursor: pointer; padding: 0.2rem;
    text-align: center; border-radius: 4px; font-size: 0.75rem;
    color: var(--text); position: relative;
  }
  .mini-cal-day:hover { background: var(--bg-hover); }
  .mini-cal-day.out-of-month { color: var(--text-muted); opacity: 0.4; }
  .mini-cal-day.today { font-weight: 700; color: var(--accent); }
  .mini-cal-day.selected { background: var(--accent); color: #fff; }
  .mini-cal-day.has-event::after {
    content: '';
    display: block;
    width: 4px; height: 4px;
    background: var(--accent);
    border-radius: 50%;
    position: absolute;
    bottom: 1px; left: 50%;
    transform: translateX(-50%);
  }
  .mini-cal-day.selected.has-event::after { background: #fff; }
</style>
