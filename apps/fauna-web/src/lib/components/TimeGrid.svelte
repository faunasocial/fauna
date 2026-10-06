<script lang="ts">
  import { onMount } from 'svelte';
  import { dayColumnLayout } from '$lib/caltime';
  import { t } from '$lib/i18n/strings';
  import { IDS } from '$lib/generated/uiIds';

  // The Outlook-style week/day time grid (events.md § Week & day timeline views).
  // ONE renderer drives both the week view (7 day-columns) and the day view (a
  // single full-width column) — the caller passes `dates` (7 vs 1), exactly like
  // linux's shared `build_day_column_fixed` feeds both `week_grid` + `day_grid`.
  // The all-day/timed classification, timed minute geometry, and overlap
  // column-packing are shared Rust (`fauna_core::caltime::day_column_layout`,
  // via `$lib/caltime`); the gutter, columns and pixel-positioning are this
  // per-app rendering shell.

  interface EventItem {
    id: string;
    summary: string;
    dtstart: string; // ISO; parsed in LOCAL time (web timestamps are tz-less)
    dtend: string;
    color?: string;
  }

  interface Props {
    /** The day-columns to render: 7 for the week view, 1 for the day view. */
    dates: Date[];
    events: EventItem[];
    onSelectEvent?: (eventId: string) => void;
    /** Clicking an empty time slot → new-event compose prefilled at that slot
     *  (the Outlook drill-in, mirroring the month grid's empty-cell behaviour). */
    onEmptySlot?: (date: Date, hour: number, minute: number) => void;
  }

  let { dates, events, onSelectEvent, onEmptySlot }: Props = $props();

  // 48px per hour (24px per half-hour) — the column is 24h tall. Block
  // top/height come from start/duration minutes; the same minute→pixel scale
  // drives the 8am auto-scroll below.
  const HOUR_PX = 48;
  const HALF_HOUR_PX = HOUR_PX / 2;
  const QUARTER_HOUR_PX = HOUR_PX / 4;
  const MIN_BLOCK_PX = HALF_HOUR_PX;
  const HOURS = Array.from({ length: 24 }, (_, i) => i);

  // Empty-slot quick-create targets (events.md § Week & day timeline views,
  // ui.yaml `events-time-slot-{HH-MM}`, approved 2026-08-01): 96 fixed
  // 15-minute markers tiling the 24h column, each carrying its own
  // closure-captured time — mirrors linux's `add_time_slot_markers`. Same
  // set for every day column, so it's computed once, not per-column.
  const SLOTS = Array.from({ length: 96 }, (_, i) => {
    const totalMin = i * 15;
    const hour = Math.floor(totalMin / 60);
    const minute = totalMin % 60;
    return {
      hour,
      minute,
      hh: String(hour).padStart(2, '0'),
      mm: String(minute).padStart(2, '0'),
      top: i * QUARTER_HOUR_PX,
    };
  });

  // The current-time line tracks wall-clock; refresh once a minute so it (and
  // the today highlight) stay correct without a reload.
  let now = $state(new Date());
  onMount(() => {
    const timer = setInterval(() => {
      now = new Date();
    }, 60_000);
    return () => clearInterval(timer);
  });

  function localDateKey(d: Date): string {
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }

  function isToday(d: Date): boolean {
    return (
      d.getFullYear() === now.getFullYear() &&
      d.getMonth() === now.getMonth() &&
      d.getDate() === now.getDate()
    );
  }

  function dowLabel(d: Date): string {
    return d.toLocaleDateString(undefined, { weekday: 'short' });
  }

  function hourLabel(h: number): string {
    return `${String(h).padStart(2, '0')}:00`;
  }

  function timeLabel(min: number): string {
    return `${String(Math.floor(min / 60)).padStart(2, '0')}:${String(min % 60).padStart(2, '0')}`;
  }

  interface PositionedBlock {
    ev: EventItem;
    top: number;
    height: number;
    leftPct: number;
    widthPct: number;
    startMin: number;
  }

  // Lay out one day's events: filter to the day (client-side), then let
  // shared Rust classify all-day vs. timed, derive timed minute geometry, and
  // column-pack overlaps in ONE `dayColumnLayout` call — so identical events
  // lay out identically to the native apps (an event crossing midnight
  // renders start → midnight everywhere, end clamped to 1440, never the
  // collapsed 30-minute block a local minutes-of-day derivation produces).
  function dayLayout(date: Date): { timed: PositionedBlock[]; allDay: EventItem[] } {
    const dk = localDateKey(date);
    const dayEvents = events.filter((e) => localDateKey(new Date(e.dtstart)) === dk);
    const placements = dayColumnLayout(dayEvents.map((e) => [e.dtstart, e.dtend]));
    const timed: PositionedBlock[] = [];
    const allDay: EventItem[] = [];
    dayEvents.forEach((ev, i) => {
      const p = placements[i];
      if (p.allDay) {
        allDay.push(ev);
        return;
      }
      const total = Math.max(p.totalColumns, 1);
      const widthPct = 100 / total;
      timed.push({
        ev,
        top: (p.startMin / 60) * HOUR_PX,
        height: Math.max(((p.endMin - p.startMin) / 60) * HOUR_PX, MIN_BLOCK_PX),
        leftPct: p.columnIndex * widthPct,
        widthPct,
        startMin: p.startMin,
      });
    });
    return { timed, allDay };
  }

  let columns = $derived(
    dates.map((date) => {
      const { timed, allDay } = dayLayout(date);
      return { date, today: isToday(date), timed, allDay };
    }),
  );

  let nowTop = $derived((now.getHours() * 60 + now.getMinutes()) / 60 * HOUR_PX);

  // Auto-scroll the timed grid to ~08:00 on first render (Outlook default), so
  // the work-day is in view without scrolling.
  let bodyEl = $state<HTMLDivElement | undefined>(undefined);
  onMount(() => {
    if (bodyEl) bodyEl.scrollTop = 8 * HOUR_PX;
  });

</script>

<div class="time-grid" class:single={dates.length === 1} style="--ncols: {dates.length}; --hour-px: {HOUR_PX}px; --half-hour-px: {HALF_HOUR_PX}px; --quarter-hour-px: {QUARTER_HOUR_PX}px;">
  <!-- Day header row -->
  <div class="grid-row grid-header">
    <div class="gutter-cell"></div>
    {#each columns as col}
      <div class="day-header" class:today={col.today}>
        <span class="dow">{dowLabel(col.date)}</span>
        <span class="dom" class:today={col.today}>{col.date.getDate()}</span>
      </div>
    {/each}
  </div>

  <!-- All-day band (always present per the contract; empty when no all-day events) -->
  <div class="grid-row allday-band" data-testid={IDS.CALENDAR_ALLDAY_BAND}>
    <div class="gutter-cell allday-label">all-day</div>
    {#each columns as col}
      <div class="allday-col">
        {#each col.allDay as ev}
          <button
            class="allday-chip"
            style="background: {ev.color || 'var(--accent)'};"
            onclick={() => onSelectEvent?.(ev.id)}
            title={ev.summary}
          >{ev.summary}</button>
        {/each}
      </div>
    {/each}
  </div>

  <!-- Scrollable timed grid: hour gutter + day columns -->
  <div class="grid-row grid-body" bind:this={bodyEl}>
    <div class="hour-gutter">
      {#each HOURS as h}
        <div class="hour-label">{hourLabel(h)}</div>
      {/each}
    </div>
    {#each columns as col}
      <div class="day-column">
        <div class="column-bg" aria-hidden="true"></div>
        {#each SLOTS as slot (slot.hh + slot.mm)}
          <button
            class="time-slot"
            data-testid="events-time-slot-{slot.hh}-{slot.mm}"
            aria-label={t.events.new_event}
            style="top: {slot.top}px;"
            onclick={() => onEmptySlot?.(col.date, slot.hour, slot.minute)}
          ></button>
        {/each}
        {#if col.today}
          <div class="current-time" data-testid={IDS.CALENDAR_CURRENT_TIME} style="top: {nowTop}px;"></div>
        {/if}
        {#each col.timed as b (b.ev.id)}
          <button
            class="event-block"
            data-testid={IDS.CALENDAR_EVENT_BLOCK}
            style="top: {b.top}px; height: {b.height}px; left: {b.leftPct}%; width: calc({b.widthPct}% - 3px); background: {b.ev.color || 'var(--accent)'};"
            onclick={(e) => { e.stopPropagation(); onSelectEvent?.(b.ev.id); }}
            title={b.ev.summary}
          >
            <span class="block-time">{timeLabel(b.startMin)}</span>
            <span class="block-summary">{b.ev.summary}</span>
          </button>
        {/each}
      </div>
    {/each}
  </div>
</div>

<style>
  .time-grid {
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
  }
  .grid-row {
    display: grid;
    grid-template-columns: 3.5rem repeat(var(--ncols), 1fr);
  }
  .gutter-cell { border-right: 1px solid var(--border); }
  .grid-header { border-bottom: 1px solid var(--border); }
  .day-header { text-align: center; padding: 0.25rem; font-size: 0.8rem; }
  .day-header.today { background: var(--bg-hover); }
  .dow { color: var(--text-muted); font-size: 0.7rem; }
  .dom { display: block; font-weight: 600; }
  .dom.today { color: var(--accent); }

  .allday-band {
    border-bottom: 1px solid var(--border);
    min-height: 1.5rem;
  }
  .allday-label {
    font-size: 0.65rem;
    color: var(--text-muted);
    text-align: right;
    padding: 0.25rem 0.25rem 0 0;
  }
  .allday-col {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 0.125rem;
    border-left: 1px solid var(--border);
  }
  .allday-chip {
    border: none;
    border-radius: 3px;
    color: #fff;
    font-size: 0.7rem;
    padding: 0.0625rem 0.25rem;
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
    opacity: 0.9;
  }
  .allday-chip:hover { opacity: 1; }

  .grid-body {
    max-height: 600px;
    overflow-y: auto;
  }
  .hour-gutter { border-right: 1px solid var(--border); }
  .hour-label {
    height: var(--hour-px);
    font-size: 0.65rem;
    color: var(--text-muted);
    text-align: right;
    padding-right: 0.25rem;
    box-sizing: border-box;
  }
  .day-column {
    position: relative;
    height: calc(24 * var(--hour-px));
    border-left: 1px solid var(--border);
  }
  /* Hour + half-hour grid lines painted as gradients on a purely decorative,
     non-interactive layer — the click targets are the .time-slot markers
     above it (events-time-slot-{HH-MM}), one real button per 15-min slot. */
  .column-bg {
    position: absolute;
    inset: 0;
    width: 100%;
    pointer-events: none;
    background-image:
      repeating-linear-gradient(to bottom, var(--border) 0, var(--border) 1px, transparent 1px, transparent var(--hour-px)),
      repeating-linear-gradient(to bottom, var(--border) 0, var(--border) 1px, transparent 1px, transparent var(--half-hour-px));
    background-position: 0 0, 0 0;
    opacity: 0.6;
  }
  /* One button per 15-min slot, tiling the 24h column beneath the event
     blocks (which paint on top via z-index below and so win the click). */
  .time-slot {
    position: absolute;
    left: 0;
    width: 100%;
    height: var(--quarter-hour-px);
    border: none;
    margin: 0;
    padding: 0;
    cursor: pointer;
    background-color: transparent;
  }
  .current-time {
    position: absolute;
    left: 0;
    right: 0;
    height: 2px;
    background: #e5484d;
    z-index: 2;
    pointer-events: none;
  }
  .event-block {
    position: absolute;
    border: none;
    border-radius: 3px;
    color: #fff;
    font-size: 0.7rem;
    padding: 0.125rem 0.25rem;
    overflow: hidden;
    cursor: pointer;
    opacity: 0.92;
    z-index: 1;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    text-align: left;
    box-sizing: border-box;
  }
  .event-block:hover { opacity: 1; }
  .block-time { font-size: 0.6rem; opacity: 0.85; }
  .block-summary {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
</style>
