# Value formatting (relative time, byte sizes, durations)

Owns: value-formatting
Status: ratified — every contract landed in shared Rust; per-app adoption matrix (now 7 apps, tui included) + the open consume legs in § Implementation status today (re-verified 2026-08-14; § Absolute local timestamp display gained its date-only sibling + ms door 2026-08-17, then the full-form ms door + the date-only UniFFI exports + windows' date adoption later the same day, sweep; § Tier rank added 2026-08-17; § Absolute local timestamp display's adoption claim stopped being a human sweep 2026-08-21 — `local-date-ratchet-check` guards windows/apple/android the way tui and linux guard themselves)
Authority: the shared value-formatting + input-validation contracts (`fauna_core::format` + `fauna_provisioning::progress`) — bucketing thresholds, rounding rules, canonical display forms, parse/validation semantics, and which decisions are `LocalizedText` vs plain values; apps MUST NOT hand-roll them. Field *specs* stay with their page/feature owners (`../ui/mail-settings.md`, `admin.md`, `../ui/feed.md`, …); i18n key content → `i18n/strings/en.yaml`.

How apps turn raw scalar values — epoch timestamps, byte counts, elapsed
seconds — into the short human strings shown in the UI (a post's "2h ago", a
file's "1.5 MB", a server's "5d 2h 30m" uptime).

**Where logic lives:** shared Rust (`fauna-core::format`), per priority #2. The
*decision* (which time bucket, which 1024-unit, which duration units) is computed
once in Rust and returned as an i18n key + args via [`fauna_core::localized::LocalizedText`]
(the same carrier shared state machines already use); each app renders the
key through its own localization pipeline (Web `L()`, Apple `Bundle`, Android
`getString`, Linux/tui `fauna_i18n` (same crate), Windows resource lookup). Apps MUST NOT
hand-roll the bucketing thresholds or the English strings.

## Relative time

`fauna_core::format::relative_time(now_ms, then_ms) -> RelativeTimestamp`, an
enum with uniform thresholds across all 7 apps:

| Elapsed (`now - then`) | Variant | i18n key (abbreviated style) |
|---|---|---|
| `< 60 s` | `JustNow` | `time.just_now` → "just now" |
| `< 60 min` | `MinutesAgo(n)` | `time.minutes_ago` → "{count}m ago" |
| `< 24 h` | `HoursAgo(n)` | `time.hours_ago` → "{count}h ago" |
| `< 7 d` | `DaysAgo(n)` | `time.days_ago` → "{count}d ago" |
| `≥ 7 d` | `Absolute { epoch_ms }` | *(none — app formats an absolute date with its native, locale-aware date formatter)* |

`RelativeTimestamp::to_localized() -> Option<LocalizedText>` maps the four
relative buckets to their key+`{count}` arg and returns `None` for `Absolute`
(the signal to the app: "render a real date with your platform's localized
date formatter"). The abbreviated forms ("5m") need no plural variants, which
keeps the i18n keys plural-free. Future-only timestamps (`then > now`) clamp to
`JustNow`.

### Resolving a two-level display — the `*_text` doors (native Rust apps)

Several decisions in this doc are deliberately handed back **two-level**: an
outer `label: LocalizedText` carrying a `{when}` / `{held}` / `{cap}` slot, plus
the inner part(s) the caller resolves first. That split is required — a
`LocalizedText` arg is a flat string, so the inner text must be localized before
substitution — and it does not change.

What *is* shared, since 2026-08-19, is the **resolution** of that split for the
two native Rust apps. `fauna-linux`'s `i18n` and `fauna-tui`'s `format` held
byte-identical bodies for it; the only per-app part was the lookup fn, and
`LocalizedText::resolve<F, S>` is already generic over that (the precedent is
`format::resolve_option_labels`, which has taken a lookup parameter since it
landed). So `fauna_core::format` gained a lookup-taking `*_text` door beside each
affected decision fn:

| door | resolves |
|---|---|
| `relative_time_display_text(&RelativeTimeDisplay, lookup)` | the bucket key, or `format_unix_local_date_ms` for the `≥ 7 d` arm |
| `relative_time_text(now_ms, then_ms, lookup)` | the above, straight from two timestamps |
| `backup_last_upload_text(secs, now_ms, lookup)` | `backup_last_upload_label` + its `{when}` |
| `backup_last_audit_text(secs, now_ms, lookup)` | `backup_last_audit_label` + its `{when}` |
| `backup_self_audit_text(secs, now_ms, lookup)` | `backup_self_audit_label` + its `{when}` |
| `backup_usage_text(held, cap, cap_state, lookup)` | `backup_usage_label` + its `{held}` / `{cap}` |

Two more doors of the same shape (added 2026-08-20)
live in `fauna-client-capabilities::view_model` rather than
`fauna_core::format`, because the *decisions* they resolve
(`custody_receipt_status_display`, `custody_held_bytes_display`,
`docs/goal/ui/devices.md` § Custody facet) live there — deliberately
wasm-clean, so the crate forwards `fauna-core`'s own `local-clock` feature one
hop rather than depending on it unconditionally:

| door | resolves |
|---|---|
| `custody_receipt_status_text(state, attested_at_micros, lookup)` | `custody_receipt_status_display` + its `{when}` |
| `custody_held_bytes_text(receipt, lookup)` | `custody_held_bytes_display` + its `{held}` / `{cap}` |

Both were byte-identical between `fauna-linux`'s `i18n::custody_receipt_status`
/ `custody_held_bytes` and `fauna-tui`'s
`settings::devices::receipt_status_text` / `held_bytes_text` at lift time —
the held-bytes pair had looked divergent (linux composing through a generated
string-table function, tui substituting slots) when row 342 was filed, but
both apps had already converged on the same generated composer by the time it
was picked up (the shared string-table lift), so the premise was
stale and both lifted cleanly rather than needing a richer-shape ruling.

All seven doors above are `#[cfg(feature = "local-clock")]`, **native-only**,
because each ultimately renders through a local-clock reader — the relative-
time arms through `format_unix_local_date_ms`, the two custody doors through
`format_unix_local` (§ Absolute local timestamp display). That gate is also
the boundary of the rule: **web is not
drift here.** It resolves the same two levels in `$lib/value-format.ts` with the
browser's locale-aware formatter, which is the whole reason the flattened
`RelativeTimeDisplay` door stays ungated — a wasm caller wants the display, not
the text. The FFI shells likewise compose in Swift / Kotlin / C# off the display
records; a closure cannot cross either boundary, so `*_text` is a native-Rust
convenience, never the contract.

Each app keeps only what is genuinely its own: its lookup, its argument order,
and its input adapters (tui's micros→ms step and its unset-`0` sentinel — `0` is
a legitimate instant to shared Rust, and only the caller knows its field treats
it as "never").

## Conversation timestamp

`fauna_core::format::conversation_timestamp(now_ms, then_ms, utc_offset_seconds)
-> ConversationTimestamp` — the contextual last-activity time shown on each
conversation/thread row. Unlike § Relative time (duration buckets), this is
**calendar-based** in the caller's local timezone: the app passes its current
UTC offset in seconds, because shared Rust is WASM-safe and can't read the local
zone. So a message at 23:50 the previous calendar day is `Yesterday` even though
it is < 24 h old.

| `then` (local calendar) | Variant | Rendered as |
|---|---|---|
| today (and any future) | `Today { hour, minute }` | local wall-clock (`14:30`) |
| the previous local day | `Yesterday` | `time.yesterday` → "Yesterday" |
| 2–6 local days ago | `Weekday { index }` (0 = Mon) | `time.weekday_mon`…`_sun` → "Mon"… |
| `≥ 7` local days ago | `Older { epoch_ms }` | *(none — app formats an absolute date with its native, locale-aware formatter)* |

`ConversationTimestamp::to_localized() -> Option<LocalizedText>` returns the key
for `Yesterday` / `Weekday` and `None` for `Today` / `Older`.
`conversation_timestamp_display(now_ms, then_ms, utc_offset_seconds) ->
ConversationTimestampDisplay { clock, localized, absolute_epoch_ms }` flattens it
for the FFI / wasm boundary — exactly one field is `Some`: `clock` (`"HH:MM"`,
24 h, local) for today, `localized` for Yesterday / a weekday, else
`absolute_epoch_ms` (the app formats a locale-aware date, as with
`RelativeTimeDisplay`). An app wanting a locale-aware 12 h clock for today uses
the `ConversationTimestamp::Today { hour, minute }` enum directly. Boundaries:
UniFFI `conversation_timestamp_display` (`libs/fauna-ffi/src/value_format.rs`),
wasm `conversationTimestamp` (`libs/fauna-wasm/src/lib.rs`). This is the canonical
shape for the conversation/DM-row timestamp on all 7 apps — it replaces the
prior per-app divergence: Linux's UTC `HH:MM` (a bug — it ignored the local
zone), Android's relative-duration, and Apple's own contextual `shortTimestamp`.

## Absolute local timestamp display

`fauna_core::format::format_unix_local(secs: i64) -> String` — a fixed,
non-localized `"YYYY-MM-DD HH:MM"` local wall-clock render, for
audit/technical timestamps that intentionally do **not** localize or bucket:
a mail credential's created-at, a nest-trust grant's `lasts_until`, a
nest-trust History-lens event's `at`. Falls back to the raw number when the
local offset is ambiguous (a DST fall-back hour) or the value is out of
range — never invents a time.

This is a deliberate exception to the "shared Rust is clock-free and
WASM-safe" rule the rest of this doc follows (§ Relative time / §
Conversation timestamp both hand the app a `utc_offset_seconds` /
`now_ms` because Rust can't read the local zone): `format_unix_local` reads
the machine's actual local timezone via `chrono::Local`, which needs the OS
timezone database — unreachable from `wasm32-unknown-unknown` without a
JS-interop bridge. So it sits behind `fauna-core`'s `local-clock` Cargo
feature, **off the default (WASM-safe) surface** — native apps opt in
(linux, tui add it directly to their `fauna-core` dependency; `fauna-ffi`
forwards it via its own `value-format` feature for windows/apple/android).
Web/wasm never enable it and keeps rendering these fields however it
currently does.

**The app-side one-door rule (ratified 2026-08-19).** "The
app supplies the offset" also means "the app supplies it from ONE place":
each app derives the device UTC offset in exactly one named function and
every other site calls it — the per-platform *sourcing* stays per-app by
design (tui `chrono`, linux glib, web `Date`, android `java.time.ZoneId`,
apple `TimeZone`, windows `TimeZoneInfo`; the platform API genuinely
differs), but a second derivation inside one app is drift waiting to
disagree. The value is not display-only — it rides
`fauna.family.usage_report` / `notify_report` and the nest persists it as
the ward-local day bucket — so two in-app producers can make the day the
nest stores differ from the day the app renders (android measurably did,
deriving through both `ZoneId` and the legacy `java.util.TimeZone`; `ZoneId`
is the ratified door there).

**Why centralize a fixed, non-localized format instead of a bucketed
decision + client-formatted string (the § Relative time / § Conversation
timestamp shape)?** Because three apps (linux — twice, independently,
within its own codebase — and windows) had already converged byte-for-byte
on this exact format + fallback behavior before this section existed, purely
by independent hand-rolling; tui (the 7th app, in buildout) added a
fourth copy, its own doc comment explicitly noting the duplication but
declining to lift it ("not worth bending `fauna_core::format` for" — written
before the `local-clock`-as-opt-in-feature option was considered).
Centralizing removes the duplication without touching the design: the
rendered output is byte-identical before and after. Android and web display
this same underlying data with their own locale-native formatters instead (a
pre-existing, separate divergence this lift does not resolve or touch — see
§ Implementation status today).

Boundary: UniFFI `format_unix_local` + `format_unix_local_ms`
(`libs/fauna-ffi/src/value_format.rs`, gated by `value-format`'s
`local-clock` forward). No wasm export — web does not consume this.
`format_unix_local_ms(ms)` (added 2026-08-17, sweep) is the
epoch-milliseconds door over the same render, flooring with `div_euclid`
exactly like the date-only sibling's ms door below and for the same reason:
while the seconds entry point was the only door, every ms-valued caller
wrote its own ms→secs chain. **Adopted:** linux (both call sites), tui (its
sole call site), and windows (2026-07-18) — all local copies deleted.
Windows had **three** independent copies of this exact shape, not one:
`NestTrustFormat.FormatLocalTimestamp` (nest-trust grant/history rows,
`NestTrustFormat.HistoryLine` + `NestsPanel.ToGrantItem`) and a separate
`MailSettingsPanel.FormatUnixLocal(ulong)` (mail-credential created-at) —
both deleted, all three call sites now route through
`FaunaFfiMethods.FormatUnixLocal`.

⚠ The sentence that used to end the paragraph above — "No native app
hand-rolls this format anymore" — went stale within a month, the same
per-function adoption-claim decay sweep diagnosed: by 2026-08-17 windows
had grown **three new** hand-rolls of this exact format
(`BackupsViewModel.FormatEpochSeconds`, `MediaPage.FormatTimestamp` — both
seconds — and `AdminBridgesPendingPage.FormatFirstSeen`, ms-valued, which is
what forced the ms door). All three re-routed through the shared fns
2026-08-17 (sweep). **linux is guarded too (2026-08-18, row 166):** a
sibling tree-walking test, `fauna-linux`'s own
`i18n::tests::no_painted_text_hand_rolls_a_shared_owned_local_date`, red-verified
against a temporarily-injected hand-roll.

**The claim is now MECHANIZED on the other three native apps too
(2026-08-21) — this cell no longer needs a human sweep.** Until then this
paragraph ended by saying the flat "nobody hand-rolls this anymore" claim was
"un-guarded on windows/apple/android … so expect this cell to need
re-verification whenever a windows surface grows a technical timestamp" — a
prediction that had already come true twice (sweep's three copies, then
sweep's five), and the most expensive shape a claim can take: one that
licenses shipping a mechanism untested, inherited by everyone who reads it. So
the sweep became a gate.
`local-date-ratchet-check` (a dedicated dev-fleet checker,
stdlib-only, measured 0.49–0.59s warm, cheap tier of **both** merge scripts — see
[`merge-gates.md`](../architecture/merge-gates.md) § Local-merge gates) walks
windows/apple/android and reds on either shared-owned shape spelled as a
date-formatter pattern. Three properties are load-bearing and each is pinned by
the gate's own tests:

- **Exact-match on the whole literal**, the same rule tui's guard uses. An ISO
  interchange pattern (`yyyy-MM-dd'T'HH:mm:ss`) is a different string, so the
  wire sites stay invisible — verified live, since `TimeGridLayout.cs`,
  `EventDateInput.swift` and `EventsScreen.kt` all spell one today and none is
  a site.
- **A locale-aware style names no fixed pattern**, so apple's and android's
  declared locale-native divergence (recorded below) is out of reach by
  construction, not by exemption.
- **Down-only per-file ratchet**, because element/automation IDs, parse tables
  and wire fragments legitimately spell these patterns and no static rule
  separates them across three languages. The 2026-08-21 survey found exactly
  **six** sites and all six are one of those three classes — sweep really
  had cleaned every render — so the baseline is non-render residue rather than
  grandfathered debt, and each entry carries its reason in the baseline's
  `_classification`. **Five remain as of 2026-08-26** — see below.

Red-verified the way linux's guard was: reverting `BackupsViewModel`'s
`FormatEpochSeconds` to its pre-sweep-166 body reds the gate on that file:line
and names the owning door. **web is deliberately unscanned** — no wasm export of
this pair exists (below), so flagging it would flag code with no shared door to
route to; widening `SCAN_ROOTS` belongs in the same change that adds the export.
**Resolved 2026-08-26 (win):** the sixth site, `EventsPage.xaml.cs`'s
`AutomationProperties.SetName(…, "yyyy-MM-dd")` on the month-grid day cell, was
frozen for a Windows session to re-decide (an accessible name *is* spoken to a
screen-reader user, so it read as the least clearly non-render of the six) —
frozen rather than fixed because windows neither compiles nor e2e-runs on the
machine the gate was written on. Ruling: it does **not** want this section's
shared door. The cell's date is a purely local calendar value assembled from
`year, month, day` with no epoch/timezone origin at all (unlike every other
site this section governs, which all start from a wire epoch); routing it
through `format_unix_local_date` would mean converting to epoch seconds and
back for no reason, which is exactly the invented round-trip § *Why the ms
door is part of the contract* warns against, just in the opposite direction.
Instead brought in line with its own sibling in the same file
(`BuildDayColumn`'s week-grid cell, which already speaks a friendly
`"Day {date:ddd d}"` name, not ISO): the month cell's name is now .NET's
culture-aware long-date pattern (`ToString("D")`), consistent with this
project's `CultureInfo`-driven windows date localization elsewhere
(`events.md` § Week & day timeline views) — which also means it
no longer matches this gate's flagged pattern (baseline count 3 → 2,
the hand-roll lint's own baseline file).

### The date-only sibling, and its milliseconds door

Two more entry points share this section's contract, its `local-clock` gating
and its never-invent-a-time fallback — they are the same render at a coarser
precision, not separate decisions:

- `format_unix_local_date(secs: i64) -> String` — a local `"YYYY-MM-DD"`, for
  compact date *fields* (a cert's expiry, a bunker connection's expiry) and for
  the § Relative time `≥ 7 d` `Absolute { epoch_ms }` fallback on apps with no
  locale-aware date widget to defer to. `%Y-%m-%d` has no locale-varying
  component, which is what makes it a shared-Rust target where a locale-aware
  short form (glib's `%b %-d`) legitimately stays platform-side.
- `format_unix_local_date_ms(ms: i64) -> String` — the epoch-**milliseconds**
  adapter over it, because most callers of the date-only form arrive from an
  ms-valued source (`Absolute { epoch_ms }`, the § Backup destination status
  labels rows' `absolute_epoch_ms` arm, a conversation list's older-message
  date). The ms→secs step floors (`div_euclid`) rather than truncating toward
  zero: `-1_500 ms` is inside second `-2`, and a plain `/ 1_000` says `-1`.

**Why the ms door is part of the contract rather than a caller's one-liner:**
because while it was missing, every ms-valued caller wrote its own chain, and
they did not agree with this section. A private
`DateTime::from_timestamp_millis(..).with_timezone(&Local).format(..)` ends
`.unwrap_or_default()`, so out of chrono's range it renders the **empty
string** — and empty is how an *unset* timestamp reads, making a corrupt
timestamp indistinguishable from a missing one. That is the opposite of "never
invents a time": it invents an absence. tui carried five such copies until
2026-08-17; linux never did, because it had already single-sourced its ms
callers through one `i18n::local_date` delegate.

**Adopted:** linux (`i18n::local_date`, one line over the shared fn — every ms
caller routes through it), tui (all five former private copies deleted:
`format::{format_epoch_us, epoch_secs_date, backup_last_upload,
backup_last_audit}` and `conversations::local_short_date`), and windows
(2026-08-17, sweep — five fixed-`"yyyy-MM-dd"` hand-rolls found behind a
"no consumer surface yet" matrix cell: `AdminDnsPage.FormatCertExpiry`
(seconds) plus four **byte-identical** private `FormatMillisLocal` copies in
the mail-aliases/spam/lists/list-members VMs; all five now route through the
UniFFI exports). Guarded on tui by `format.rs`'s
`no_painted_text_hand_rolls_a_shared_owned_local_date`, a
tree-walking test that reds on any `.format(..)` of either shared-owned shape
anywhere under `src/` — the sibling of the month/weekday-name guard beside it,
on the duplication axis rather than the i18n axis. **Both** shapes this section
owns — the date-only form here and the datetime form above — are the same two
needles the cross-language gate carries, so windows/apple/android are guarded on
this form too.
Boundary: UniFFI
`format_unix_local_date` + `format_unix_local_date_ms`
(`libs/fauna-ffi/src/value_format.rs`, added 2026-08-17 with the windows
consume — before that the pair had no export at all). No wasm export:
apple/android and web render this same data with their own locale-native
date formatters (the same pre-existing divergence § Implementation
status today records for `format_unix_local` — apple's surfaces verified
locale-native 2026-08-17, sweep).

## Byte sizes

`fauna_core::format::byte_size(bytes) -> LocalizedText`: pick the largest
1024-unit whose value is `≥ 1`, format the scaled value to at most one decimal
(trailing `.0` dropped), and return the unit key with a `{value}` arg:

| Range | i18n key |
|---|---|
| `< 1 KiB` | `size.bytes` → "{value} B" |
| `< 1 MiB` | `size.kb` → "{value} KB" |
| `< 1 GiB` | `size.mb` → "{value} MB" |
| `< 1 TiB` | `size.gb` → "{value} GB" |
| `≥ 1 TiB` | `size.tb` → "{value} TB" |

Unit symbols (B/KB/MB/…) are locale-invariant. The scaled number is formatted
Rust-side with a `.` decimal point; apps render `{value}` verbatim.

## Tip amounts and counts

The post tip surface's two display decisions ([`monetization.md`](monetization.md)
§ Tips owns the feature; this section owns how the numbers are spelled).

`fauna_core::format::tip_amount(msats) -> LocalizedText` — **sats are the display
unit, msats the wire unit.** § The asking price already fixes that split for the
author's input; a reader's total is the same split read backwards.

| Range | i18n key |
|---|---|
| `< 1 sat` (`< 1000 msat`) | `tips.msats` → "{value} msats" |
| `≥ 1 sat` | `tips.sats` → "{value} sats" |

Scaled to at most one decimal with a trailing `.0` dropped, exactly like
§ Byte sizes; unit symbols are locale-invariant. Sub-sat amounts keep their own
unit rather than rounding to "0 sats" — the same reason byte sizes keep bytes
below a KiB, except here the stake is higher: a zero would be a lie about real
money.

`fauna_core::format::tip_count(count) -> LocalizedText` — `tips.count_one`
("1 tip") at exactly 1, else `tips.count` ("{count} tips"). Shared because the
singular is a **decision, not a formatting detail**: seven apps each picking one
is seven chances to ship "1 tips". The codebase has no plural machinery, so the
two forms are two keys (the `time.hour_1` precedent).

**Callers render an amount only when the total is non-zero.** A post every one of
whose receipts carried an unparseable invoice has real tips and no summable
amount, so `post-tip-count` renders and `post-tip-total` does not — the count and
the amount move independently, which is why they are separate elements
(`monetization.md` § Tips: a missing amount is a real state, never coerced to 0).

## Event count

`fauna_core::format::event_count(count) -> LocalizedText` — `events.event_count_one`
("1 event") at exactly 1, else `events.event_count` ("{count} events"), the
`tip_count` shape reused for the month-grid `events-day-cell` accessibility
tooltip ("Monday March 16, 3 events", `events.md` § Layout & flow). Same
reason: the singular is a decision, not a formatting detail, and the codebase
has no plural machinery, so the two forms are two keys. **Callers render the
count clause only when `count > 0`** — a dayless clause ("Monday March 16")
is a caller-side choice, this function's job starts once there is a count to
speak.

## Duration / uptime

`fauna_core::format::duration_secs(secs) -> LocalizedText`: a coarse
`d`/`h`/`m` breakdown (server uptime, sync duration, …) — the largest non-zero
unit down to minutes, always showing the full chain below it. Sub-minute
durations render as "0m". Seconds are intentionally dropped (coarse display).

| Magnitude | i18n key | args |
|---|---|---|
| `≥ 1 d` | `time.uptime_dhm` → "{days}d {hours}h {mins}m" | `days`, `hours`, `mins` |
| `≥ 1 h` (`< 1 d`) | `time.uptime_hm` → "{hours}h {mins}m" | `hours`, `mins` |
| `< 1 h` | `time.uptime_m` → "{mins}m" | `mins` |

Unit symbols (d/h/m) are locale-invariant. This adopts the always-full chain
(linux's prior form) over the truncated `{d}d {h}h` some apps used (e.g.
windows-ctl dropped minutes once days were present) — priority #4, richest
pattern wins.

## Grace countdown

`fauna_core::format::grace_countdown(deadline_ms, now_ms) -> Option<LocalizedText>`:
a coarse `d`/`h` countdown to a future deadline (no minutes — the one caller
today, the mail primary-domain-rename `admin-dns-rename` grace-window banner,
is days-scale and re-renders on each snapshot refresh, not a per-second
ticker). `None` once `now_ms >= deadline_ms`; the caller renders its own
already-localized "elapsed" label for that state (e.g.
`admin.dns.rename.grace_elapsed`) — the wording is per-surface, so the shared
fn only owns the still-counting-down branch.

| Magnitude | i18n key | args |
|---|---|---|
| `≥ 1 d` remaining | `time.countdown_dh` → "{days}d {hours}h" | `days`, `hours` |
| `< 1 d` remaining | `time.countdown_h` → "{hours}h" | `hours` |
| elapsed (`≤ 0`) | — (`None`; caller's own elapsed label) | — |

Before this lift, web (`graceRemaining`), linux (`grace_remaining`), apple,
android, and windows each independently hand-rolled the identical `ms →
days/hours → "{days}d {hours}h"` arithmetic with the unit letters baked in as
literal English — a priority #1 violation on every app, not just web (the
five implementations' own doc comments cross-referenced each other, evidence
this was hand-copied rather than derived independently). Unified onto this fn
(priority #1/#2/#4). Exposed as UniFFI `grace_countdown`
(`libs/fauna-ffi/src/value_format.rs`, `value-format` feature — dropped from
the Go mail-bridge's `--no-default-features` build, no binding churn) and wasm
`graceCountdown` (`libs/fauna-wasm/src/lib.rs`); linux calls
`fauna_core::format::grace_countdown` directly (native Rust, no FFI hop).

## Provisioning elapsed

The onboarding `provisioning-elapsed` ticker ("{seconds}s elapsed") is a fourth
value-formatting case. Unlike the three above it lives in
`fauna_provisioning::progress::elapsed_display(started_at_ms, finished_at_ms,
now_ms) -> Option<LocalizedText>` — *not* `fauna_core::format` — because the
*decision* is snapshot-specific: `None` until the run starts (the row stays
hidden), freeze at `finished_at_ms` once set (the ticker stops on
Succeeded/Failed/Cancelled), otherwise tick against the app's live `now_ms`,
saturating on a backwards clock. It returns the canonical
`onboarding.nest_provisioning.elapsed_template` key + `{seconds}` arg, so every
app renders the same string through its own i18n pipeline (no hand-rolled
English — same rule as above). It's a free function, not a `Record` method,
because the snapshot crosses the uniffi/wasm boundary as a `Record` and apps
already hold the two timestamps; `now_ms` is the app's live tick. Exposed at
both boundaries as UniFFI `provisioning_elapsed` (`libs/fauna-ffi/src/provisioning.rs`)
and wasm `provisioningElapsed` (`libs/fauna-wasm-onboarding/src/lib.rs`).

Enriching the format to a minutes+seconds breakdown (e.g. "1m 5s" for
multi-minute provisions) is a deferred, cheap toggle now that all apps route
through the one fn — a single en.yaml key + `elapsed_display` edit, no per-app
hunt. The current canonical raw-seconds form is what 6/7 apps already use.

## Provisioning step display

The onboarding `provisioning-step-row` presentation — the status glyph, the step
name, and the sub-step text — is single-sourced in `fauna_provisioning::progress`
(the same crate and `LocalizedText` carrier as [Provisioning
elapsed](#provisioning-elapsed) above), since the decision is likewise
snapshot-specific. The five-to-six per-app re-implementations (each a "twin of
`nest_provisioning.rs`") collapse to:

- `status_glyph(StepStatus) -> String` — the `provisioning-step-checkbox` glyph
  (`○`/`…`/`—`/`✓`/`✗`). A plain `String` (a locale-invariant symbol, nothing to
  translate — same rationale as [Short id](#short-id)). The **canonical set is
  linux/apple's `○ … — ✓ ✗`**, resolving the prior 2-2 drift where windows+web
  used `○ ⟳ − ✕`: `…`/`—`/`✗` have broader platform-font / assistive-tech coverage
  than the gapped-circle-arrow / minus-sign / multiplication-x, and em-dash +
  ballot-X are the more conventional "skipped"/"failed" marks (`○`/`✓` already
  agreed everywhere).
- `step_label(ProvisionStep) -> LocalizedText` — the step name's i18n key. The
  **canonical key family is `onboarding.provision.step.*`** (the cohesive home
  alongside `substep.*` / `step_failed` / `step_attempt_template`); the
  identical-valued `onboarding.nest_provisioning.step.*` entries were a stray
  duplicate, **removed 2026-06-14** once android (the last remaining consumer)
  swapped onto `provisioning_step_label`.
- `substep_label(SubstepKey, cause: Option<String>) -> LocalizedText` — the
  sub-step text's i18n key. For `StatusRetrying` the `{cause}` arg is filled from
  the step's `last_error` (`status_retrying` = "Retrying after error: {cause}") so
  every app substitutes it; previously only web did — linux/apple/windows
  showed the user a literal `{cause}`. (The Skipped-row "already configured"
  placeholder and the `step_attempt_template` suffix stay client-side text
  assembly — they concatenate two i18n strings, which one `LocalizedText` can't
  represent.)

Exposed at both boundaries as UniFFI `provisioning_status_glyph` /
`provisioning_step_label` / `provisioning_substep_label`
(`libs/fauna-ffi/src/provisioning.rs`, all behind the `value-format` gate so the
Go mail-bridge `--no-default-features` build drops them) and wasm
`provisioningStatusGlyph` / `provisioningStepLabel` / `provisioningSubstepLabel`
(`libs/fauna-wasm-onboarding/src/lib.rs`). Native Rust (Linux) calls
`fauna_provisioning::progress::*` directly.

## Short id

`fauna_core::format::short_id(hex) -> String`: the canonical short display form
for a long hex id — a 64-hex Fauna actor-id or nest-id. Returns the **first 12
characters followed by a single-character ellipsis `…` (`U+2026`)**; ids of 12 or
fewer characters are returned unchanged. Unlike the four cases above it returns a
plain `String`, not a `LocalizedText`: the form is locale-invariant (hex +
ellipsis, nothing to translate).

This unifies the per-app truncations that had drifted — web
`hex.slice(0,12)+'...'`, iOS `.prefix(12)+"..."`, Android `.take(12)+"..."`, Linux
`&hex[..12]+'…'` (same 12-char prefix, but the trailing punctuation split between
ASCII `...` and the `…` glyph), plus a Linux/Windows minority that used a
first-8…last-8 form for actor- and nest-ids. The canonical form is the convergent
majority (12-char prefix — web/iOS/Android/linux-admin) with the proper `…` glyph:
priority #1 (minimize per-app divergence) / #4 (resolve drift, pick the richest
*consistent* pattern). Nest-ids (the linked-nests list) take the same `short_id`;
the first-8…last-8 sites converge onto it.

Exposed at both boundaries as wasm `shortId` (`libs/fauna-wasm/src/lib.rs`) and
UniFFI `short_id` (`libs/fauna-ffi/src/identity.rs`, returning `String` — no
feature gate, as it carries no `LocalizedText`). Native Rust (Linux) calls
`fauna_core::format::short_id` directly.

## Short nest id

`fauna_core::format::short_nest_id(id: &str) -> String`: head…tail elision of a
long hex `nest_actor_id` for a box-recovery row label — the **first 8
characters, a single-character ellipsis `…` (`U+2026`), and the last 8
characters**; ids of 20 or fewer characters are returned unchanged. Plain
`String`, not `LocalizedText` (locale-invariant, nothing to translate).

Distinct from [§ Short id](#short-id) above: `short_id` keeps only a 12-char
*prefix* (no tail) and is the general handle-less actor/nest fallback label
(the linked-nests list included — see § Short id's "first-8…last-8 sites
converge onto it"). `short_nest_id` is a **separate, still-live** first-8…
last-8 form specific to the box-recovery `recover-box-item` row (`box-recovery.md`
§ Recovery UI (step 4)): until `DeploymentSeedEntry` carries the box's own
domain/label (Task C2), the 64-hex `nest_actor_id` is all the row has to show,
and a head+tail form keeps a long id recognizable in a way a 12-char prefix
alone would not. It does not converge onto `short_id` — the two serve
different UI needs and both stay.

Unifies the per-app box-recovery elisions — web `shortNestId`
(`routes/onboarding/+page.svelte`), Linux `short_nest_id` (`views/onboarding/nest_recovery.rs`),
Windows `ShortNestId` (`OnboardingViewModel.cs`, whose own comment already
flagged it as "a candidate future shared-Rust lift") — onto one shape
(priority #1/#2/#4). Exposed at the native boundary as UniFFI `short_nest_id`
(`libs/fauna-ffi/src/identity.rs`, ungated — plain `String` return, no
Go-incompatibility concern); Linux calls `fauna_core::format::short_nest_id`
directly (Rust dep). Web consumes the wasm `shortNestId` export (landed 2026-07-09).

## Fleet fingerprint

`fauna_core::format::fleet_fingerprint(id: &[u8; 32]) -> String`: the short display form of a **fleet id** — a fleet member's 32-byte device principal — on the Devices page: the **first 8 hex characters, a single-character ellipsis `…` (`U+2026`), and the last 8** over the canonical lowercase hex (`3f9a1b2c…c21e9d8f`; always 17 characters). Plain `String`, locale-invariant.

It is the ONE identity a client holds for a fleet member that no roster row accounts for ([`../architecture/account-data-taxonomy.md`](../architecture/account-data-taxonomy.md) § The generation machinery → *Fleet-scope reclamation*, clause (4), *A disagreement is the user's to settle*; the page: [`../ui/devices.md`](../ui/devices.md) § Members without a matching entry) — such a member has no nest row, so no label. The user settles a removal **by elimination**: every device still in hand shows its own fingerprint (`device-own-fingerprint`), and the member to remove is the card (`device-member-fingerprint`) matching none of them. That makes the two surfaces two halves of one comparison, so **both render through this function and nothing else** — the shared `DevicesMachine` renders both into its snapshot (`FleetMemberSummary::fingerprint`, `DevicesSnapshot::own_fingerprint`), and no app formats a key itself. Were they ever to differ, "matches none" would fire on a device the user holds.

**The width is a security parameter, not a taste.** An attacker holding a stolen device can re-mint its principal until the fingerprint collides with a live sibling's; a collision makes the stolen card read as "a device you hold" (the honest re-minted case the note warns about). Sixteen hex characters put that search at 2^64 trials; the four-and-four the design sketch showed would have been 2^32, a laptop's afternoon. So this formatter never narrows: it shares the head…tail *shape* of [§ Short nest id](#short-nest-id) but is its own function with its own width, pinned by `fleet_fingerprint_keeps_eight_hex_at_each_end`. Distinct from [§ Short id](#short-id) (a prefix only) and from § Hex id display (a hex *string* input; this takes the raw bytes and encodes canonically, so an upper-cased or non-canonical id can never render a different fingerprint).

Faces: rendered **inside the shared snapshot** — every app reads the string, none calls the formatter — so there is no per-app face to wire and nothing for the UniFFI or wasm boundaries to expose (2026-09-25).

## Account display label

`fauna_core::format::account_display_label(handle: Option<&str>, actor_id: &str)
-> String`: the account-switcher row title — the account's cached **handle**
when present and non-empty, else the canonical [`short_id`](#short-id) of its
actor id (usable before the first server-data cache refresh). Plain `String`
(locale-invariant).

Second consumer (2026-07-14): the pending-share sharer label — the
`FolderPendingShare.shared_by_display` field (`libs/fauna-client-inbox`,
mirrored on `FfiPendingShare`) is pre-computed through this fn so the
`folder-pending-share` row renders one string on all seven apps (it replaces
three drifted per-app truncations; empty string only for a fully unstamped
cross-nest share, where the app renders its unknown-sharer i18n label).
Consumed by windows the same day (`FoldersPage.xaml.cs::BuildPendingShareRow`,
`common/unknown` as the empty-string fallback label); linux 2026-07-14
(`folders.rs::build_pending_share_row`); apple/ios and android 2026-07-16;
web last, 2026-07-22 (`FoldersSection.svelte::pendingShareSharer` reads
`share.sharedByDisplay`, `t.common.unknown` fallback) — no drifted leg
remains open.

**A cross-nest sharer or owner is labelled by the canonical `handle@domain`
(ratified 2026-10-03; BUILT 2026-10-05)** — the same string the conversation roster
seats a foreign member under — never a bare handle, which everywhere in the
apps means one of the viewer's own nest's users. The pair crosses every wire
split (`shared_by_handle` + `shared_by_domain` on the Welcome,
`owner_handle` + `owner_domain` on the federated read reply and the
`ForeignFolder` record) and is joined by one shared formatter,
`fauna_core::format::qualified_handle(handle: Option<&str>, domain:
Option<&str>) -> Option<String>` (lifted from
`RoomRosterKnownMember::qualified_handle`, which then calls it), whose result
feeds `account_display_label` as the handle — so `shared_by_display` reads
`alice@example.com` on the knock and the shared set's display name reads
`docs (alice@example.com)` (`on-demand-files.md` § Shared sets on a
capability host, decision 3). Who stamps the pair, and the domain-binding
check that gates it: `../architecture/federation.md` § Cross-nest shared
folders + channel append → *The cross-nest owner label*. The empty-string
branch above stands: a share whose pair did not verify is still a fully
unstamped cross-nest share.

Third consumer (2026-07-14): the owner-side folder badge — the `FolderSummary.owner_display`
field (`fauna_devices_machine::snapshots`, computed in the `From<WireFolder>` transcribe) is
pre-computed through this fn so the recipient `folder-shared-badge` ("Shared by ‹…›") renders one
string on all seven apps. The nest stamps the owner's `actor_id` beside `owner_handle` on the wire
(`fauna_protocol::folders::FolderSummary.owner_actor_id`, additive, member rows only); the
transcribe folds handle-else-`short_id` into `owner_display`. This kills the same fallback drift the
pending-share row had — three per-app truncations (apple degraded to a bare `…`, android to a raw
12-char hex prefix); the empty string is reached only on the caller's own rows, which render
"Shared · N", not a "Shared by" badge. linux consumed it 2026-07-15
(`folders.rs::build_member_folder_row`, deleting the `owner_handle.unwrap_or("…")` fallback);
windows the same day (`FoldersPage.xaml.cs`, `fs.ownerDisplay`); apple/ios and android 2026-07-16
(shared FaunaKit renderer / `MemberFolderRow`); web last, 2026-07-22 (`FoldersSection.svelte:778`,
`fs.owner_display`) — no per-app render swap remains open.

Fourth consumer (2026-07-16): the owner-side "Shared with" roster row — the
`FfiFolderActorMember.display` field (`libs/fauna-ffi/src/folders_client.rs`, computed in the
`From<FolderActorMember>` mirror) is pre-computed through this fn so the `folder-member-handle`
row renders one string. This closes the last hand-rolled fallback on the folders § Sharing page,
and the drift it carried was the worst of the three: linux, apple and android each rendered the
**raw 64-hex actor id** when a member's handle is unset (a remote actor, or a local user with no
handle set) — an unreadable blob in a table row — while windows alone truncated it (12 chars +
`…`, i.e. `short_id` by hand). The Rust-native linux app calls this fn directly
(`views/devices_folders/folders.rs::populate_folder_actors`); apple / android / windows read the
pre-computed field over UniFFI (the roster crosses as `FfiFolderActorMember`, so the label is
computed once at that mirror) — confirmed in place on all three, no render swap remains open
(`FolderSharingViews.swift:93,116`, `FoldersScreen.kt:707`, `FoldersPage.xaml.cs:1284`). Web built
its own owner-side roster later (2026-07-21, `FoldersSection.svelte`'s "Shared with" section) with
**no** pre-computed `display` field on its wire type — the row calls
`accountDisplayLabel(member.handle, member.actor_id)` (the wasm face) directly per row instead
(`devices-machine.ts`'s `FolderActorMember` doc comment states this explicitly) — the same fn, a
different boundary shape.

Fifth consumer (2026-09-09): the conversations **participant row** — the Fauna arm of
`TypedAddress::display` (`libs/fauna-conversations/src/address.rs`) resolves through this fn.
A Fauna address seated off an MLS roster carries `handle: String::new()` (the engine roster
gives actor ids and nothing else — `conversation-rooms.md` § Implementation status today), and
the arm used to hand that empty string straight back, so a member welcomed into a group, or
added to it by someone else's device, rendered as a **blank row** in the member chips, the
thread-header participant list and the room-settings roster. This is the same unset-handle case
as the fourth consumer's, one surface over, and it reached further: an empty display also
collapsed two distinct nameless members into one under the `display()`-keyed participant dedup.
All seven apps already render the shared `display()` (natively over the `typed_address_display`
FFI export, web over wasm), so no per-app render swap was needed or left open. `person_handle()`
is its read half: a caller asking whether there is a *name* must use that, since `display()` now
always answers with something.

This resolves real drift, not just duplication (priority #4 — richest pattern
wins): Linux's `settings/account.rs` already filtered an empty-string handle
as absent and appended the `…` ellipsis on the actor-id fallback; web's
`settings/[[subpage]]/+page.svelte` (`acct.handle ?? acct.actor_id.slice(0, 12)`)
had drifted from that — no ellipsis, and `??` does not treat an empty string
as absent — despite an inline comment claiming it "mirrors the linux
reference." Lifting Linux's richer shape and routing both through it fixes
web's drift rather than merely unifying two already-correct copies.

Exposed at the native boundary as UniFFI `account_display_label`
(`libs/fauna-ffi/src/identity.rs`, ungated — plain `String` return); Linux
calls `fauna_core::format::account_display_label` directly. Web consumes over
wasm (2026-07-09) and apple's account-switcher does too (2026-07-14/15);
windows/android's are consume-from-start once their multi-account switcher
ships (§ Implementation status today).

## Peer display label

**Ratified 2026-09-19, built 2026-09-26 (§ Implementation status today).** `fauna_core::format::peer_display_label(nickname: Option<&str>, display_name: Option<&str>, handle: Option<&str>, actor_id: &str) -> PeerLabel { primary: String, public: Option<String> }` — the **one** answer to "what do I call this other person", for every surface that names someone who is not the viewer. `primary` is the first of: the viewer's own **nickname** for them (trimmed, non-empty — the private contact overlay, [`../ui/contacts.md`](../ui/contacts.md) § The private overlay, which owns *where* it paints and the two guards on it), the person's self-published **display name** (trimmed, non-empty; most surfaces hold none and pass `None` — only a surface that has fetched the `Profile` record has one), then exactly [`account_display_label(handle, actor_id)`](#account-display-label) — handle, else [`short_id`](#short-id). `public` is `Some` **only when a nickname supplied `primary`**, and then carries what `primary` would have been without it, so a surface that shows a secondary line never hides the public identity behind a private name; with no nickname it is `None` and the surface renders one line as it does today. Plain values, locale-invariant. The one surface that passes a display name without fetching a `Profile` is the feed card for a **bridged** author: `PostSummary.author_display` carries the bridge-served display name and handle into this slot ([`bridges.md`](bridges.md) § Unified feed ingestion → *Bridged authors*, ruled 2026-09-26).

It is a wrapper, not a rival: `account_display_label` keeps its own callers (the account switcher and the folder-sharing rows name *accounts and sharers by public identity* and take no nickname in v1), and a call with `nickname = None, display_name = None` is byte-identical to it — which is what lets the surfaces below migrate without a visible change until an overlay exists. Snapshot builders call it in shared Rust and hand apps the finished strings; natives read them as pre-computed snapshot fields (the `owner_display` / `shared_by_display` recipe above), and a UniFFI + wasm face exists for the row types that cross as raw fields.

**The drift it retires** (measured on tui, the lead app, 2026-09-19 — three different fallbacks for the same question): the contacts roster row hand-rolls handle-else-**raw 64-hex** (`apps/fauna-tui/src/contacts.rs` `contact_display`); the feed post author paints the **raw hex** `PostSummary.author` with no handle at all; the Profile header hand-rolls name-else-**full hex** (`profile/mod.rs` `header_text`); the knock sender and the conversations participant rows already reach `short_id`. All converge on this fn's chain.

## Device display identity

`fauna_core::format::device_display_identities(devices) -> Vec<String>`: the
display identities of **one owner's whole device list**, for a surface whose
reader holds **no key for the registering owner's label root**. One string per
input device, in order. Each is the device's **code** — the hex `device_id`,
elided to the narrowest width that tells every device in that list apart (from
the canonical [`short_id`](#short-id) width of 12 up to the full id) — with the
machine-authored plaintext label rendered beside it as `label · code` when one
rests (post-flip the only plaintext the `sync_devices.label` column ever holds —
`path-sealing.md` § Sealed names & paths). Guaranteed non-empty and **distinct
across the list**; plain `String` (locale-invariant).

⚠ **It takes the set, not one device, and must not be reduced to a per-device
function.** Distinctness is a set property: nothing handed a single device can
promise its output differs from a row it never sees. The predecessor
`device_display_identity(label, device_id)` had that signature and could not keep
the guarantee — `device_id` is **client-chosen**, so an owner shown another of
their devices could register one agreeing on the 12 rendered characters and
produce a byte-identical row, and two devices carrying the same machine label
(`SELF_REGISTER_LABEL`, what every self-registering client passes) collided with
no adversary at all. Removed 2026-08-05. Widening only on a
real collision is the deliberate trade: a fixed 12 is forgeable, a fixed 64 is
unreadable on every row forever, and set-relative width pays the legibility cost
exactly when an ambiguity exists — an owner can force it up on their own list,
which is visible and never misleading.

Sole consumer today (2026-08-02, the ruling that created it): the **nest-side**
guardian ward-device projection — `fauna.family.status`'s
`FamilyWardDeviceInfo.label` (`bins/fauna-nest/src/family_handlers.rs`) is
populated through this fn over the ward's whole device list, so the guardian's
`family-device-mark-item` rows render a non-empty, per-device-distinct identity
on all 7 apps with **zero app-side code** (the apps render the wire field
verbatim). Behavioral ruling
and its rationale: `family-safety.md` § Full visibility for young children;
the sealed-label mechanics: `path-sealing.md` § Sealed names & paths (ruled
gap (c)). Nest-side rather than a client transcribe because the substitution
must hold for every shipped client at once (an app rendering the raw scrubbed
column shows indistinguishable blank rows on a *safety* control), and no
client-held key can ever improve on it — the guardian is the one reader for
whom the seal is *permanently* unopenable, so there is no custody-aware
render an app could add.

## Subscription author label

The `subscription-mine-author` row on the `subscription-settings` page names the
creator a subscription is *to*: the viewer's own nickname for them when set (§ Peer display label, with this chooser's answer as the public name),
else the creator's nest-resolved handle when non-blank, else the **full hex** actor id. One shared chooser decides it —
`fauna_core::format::author_display_label(handle, author_id) -> String` (plain
`String`; an id and a handle are locale-invariant, nothing to translate).

**Distinct from [`account_display_label`](#account-display-label)** despite the
near-identical shape: that one falls back to `short_id` (a 12-char elision) for
the account-switcher title, whereas this site falls back to the **full** hex —
the creator's canonical, copyable identity when no handle resolves (§ Hex id
display). The two are **not** interchangeable, and the fallback is the whole
difference; do not route one through the other.

The handle is returned **trimmed**, and a whitespace-only handle counts as absent.
This resolves real drift, not just duplication (priority #4 — richest pattern
wins): linux, apple, android and windows each tested only `is_empty`/`Length > 0`,
so a handle of `"   "` rendered a **blank** author cell on four of five apps;
web alone trimmed (`sub.handle?.trim()`) and correctly fell through to the hex id.
Web's richer shape is the one lifted.

**Pre-computed at each transcribe, not exposed over UniFFI/wasm.** The label is
derived once per row where the wire type crosses into a client, mirroring
`FfiFolderActorMember.display` (§ Account display label, fourth consumer):

- **apple / android / windows** read the pre-computed `FfiMineSubscription.author_display`
  (`libs/fauna-ffi/src/subscriptions_client.rs`, computed in the
  `From<MineSubscription>` mirror — the subscription crosses as that record, so
  the label is computed once at that mirror). The module is ungated, so the field
  is also in the Go bindings; the mail-bridge does not read it.
- **web** reads the `author_display` key of the `mine.list` row JSON
  (`libs/fauna-wasm/src/rpc.rs::mine_subscription_to_web_json` — the wasm twin).
- **linux**, being Rust-native, calls `author_display_label` directly
  (`settings/subscriptions.rs::build_row`).

The compute deliberately does **not** live on the wire type
(`fauna_protocol::subscriptions::MineSubscription`): the wire carries `handle` +
`author_id` as data, and presentation is chosen at the client boundary.
Consumers render `author_display` verbatim — an app re-deriving
handle-else-hex locally is the drift this section exists to prevent.

## Hex id display

Two shared byte→hex encoders render a raw **byte** id (an actor-, member-, or
device-id `&[u8]`) as a lowercase hex string, both plain `String` (locale-
invariant, nothing to translate):

- `fauna_core::format::hex_short(bytes) -> String` — the **first 4 bytes** as 8
  lowercase, zero-padded hex chars (the short label behind a backup-restore
  source; see `docs/goal/ui/backups.md`). UniFFI face `hex_short`
  (`libs/fauna-ffi/src/identity.rs`, ungated → also in the Go bindings), wasm
  `hexShort`.
- `fauna_core::format::hex_full(bytes) -> String` — **every byte** as two
  lowercase, zero-padded hex chars: the canonical display/copy form of an id
  shown as the fallback when no handle is available. UniFFI face `hex_full`
  (`libs/fauna-ffi/src/value_format.rs`, gated behind the default-on
  `value-format` feature so the Go mail-bridge `--no-default-features` build
  drops it — no `libs/fauna-mail-go` churn; the fn is native-client-only).

Both unify the per-app byte→hex copies that had drifted only in placement, not
output — Linux `.to_hex()`, Android `HexUtil.bytesToHex` / `hexShort`, Windows
`Convert.ToHexString(..).ToLowerInvariant()` / `HexShort`, web `actorHex` /
`hexShort`, apple `hexString` — onto one source of truth (priority #1/#2/#4).
Distinct from `short_id`, which truncates a 64-hex *string* (not bytes) to 12
chars + `…`. (Crypto/wire byte→hex encodings — e.g. a secret or a persisted
device-id string — are **not** display labels and stay in client code.) **Apple adoption (2026-07-02):** apple's byte→hex *display* labels render via the shared UniFFI `hexFull(bytes:)`, dropping their private per-view byte→hex hand-rolls — `ProfileView`'s subscriber-id fallbacks (`subscription-request-subscriber` / `-subscriber-handle` / the two roster row `automationValue`s, dropping `hexString(_ Data)`), `SubscriptionSettingsView`'s author-id fallback (`subscription-mine-author`, dropping its `hexString(_ Data)`), and `AdminUsersHubView`'s actor-id labels (`invite-request-row-actor` full-hex + the `user-row` / `user-actor-id` short form's byte→hex core, dropping its `hex(_ Data)`; the local 12-char truncation stays as a distinct `short_id` concern). Apple's `Data.hexString` extension is retained for **crypto/wire** byte→hex only (e.g. `APIClient` device-id strings, `PushManager` APNs tokens, `FFICompat.data_to_hex` picker `.tag` identities), still to be consolidated onto it separately.

## URL host display

`fauna_core::format::url_host(url: &str) -> String`: best-effort host
extraction from a `scheme://host[:port]/…` URL for a display label (e.g.
`"example.com"` for `"https://example.com/article"`), scheme/port/path dropped,
no `url`-crate dependency. Returns the original string when no host can be
isolated (empty or scheme-only input). Renders the `link-preview-domain` child
of a `RenderBlock::LinkPreview` card (`docs/goal/architecture/render-model.md`
§ D4) and the onboarding self-domain label.

Unifies the per-app URL-host parses onto one source of truth (priority
#1/#2/#4): exposed at the UniFFI boundary as `url_host`
(`libs/fauna-ffi/src/value_format.rs`, ungated), consumed by android
(`com.fauna.ffi.urlHost`), apple (`ValueFormat.urlHost`, wrapping
`FaunaFFISwift.urlHost`), and windows (`FaunaFfiMethods.UrlHost`). Native Rust
(linux `main.rs`/`views/document.rs`, tui `feed/mod.rs`) call
`fauna_core::format::url_host` directly. Also exposed via wasm as `urlHost`
(`libs/fauna-wasm/src/lib.rs`) — added 2026-07-24, since web's
`LinkPreviewCard.svelte` was the only app hand-rolling its own version
(`new URL(u).hostname`) instead of consuming the already-shared fn every other
app used; web now calls `urlHost` directly. **Behavior note:** the old web
hand-roll differed from `url_host` on a scheme-less malformed input (`new
URL()` throws and falls back to the *entire* raw string, whereas `url_host`
isolates just the host portion even without a scheme) — immaterial in
practice, since every `url` reaching this component is an already-resolved
absolute link-preview URL.

## Confidence percent

`fauna_core::format::confidence_percent(per_mille: u16) -> u32`: the whole-percent
display of a moderation classifier's `confidence_per_mille` (`0..=1000` — the
dag-cbor wire form, since floats are forbidden; see `moderation.md` § State & data shape),
rounded **half-up** as `(per_mille + 5) / 10`. So `920 ‰ → 92 %`, `925 ‰ → 93 %`,
`995 ‰ → 100 %`; a well-formed input yields `0..=100`. Return is a plain `u32`
(the widening prevents overflow on an out-of-range wire value), not a
`LocalizedText` — the number is locale-invariant; the surrounding
`"{pct}% confidence"` label word is resolved through each app's own i18n
(linux `mod_strings::CONFIDENCE`, apple `L.moderation.confidence`).

This unifies the per-app copies that each re-derived the same integer rounding
with explicit "mirror" comments and duplicate tests — Windows
`(a.confidencePerMille + 5) / 10` (`ModerationViewModel`), Linux
`(action.confidence_per_mille + 5) / 10` (`views/moderation.rs`), apple
`(UInt32(perMille) + 5) / 10` (`ModerationQueueVM`) — onto one source of truth so
no app silently drifts to truncation on a shown number (priority #1 minimize
divergence / #2 shared Rust / #4 resolve drift). Mirrors the
`fauna_protocol::spam` probability↔per-mille lift, done for the same drift-closing
reason.

Exposed at the UniFFI boundary as `confidence_percent`
(`libs/fauna-ffi/src/value_format.rs`, behind the default-on `value-format` gate
so the Go mail-bridge `--no-default-features` build drops it). Native Rust (Linux)
calls `fauna_core::format::confidence_percent` directly. **Web consumes it over the
wasm boundary** as `confidencePercent` (`libs/fauna-wasm/src/lib.rs` → `$lib/wasm`
thunk): the content-label badge (`ContentLabelBadge.svelte`, the `title` tooltip on
feed posts + the moderation queue) and the Settings moderation-queue confidence
column both delegate to it. Web's local classifier holds a float, so it quantizes to
the per-mille wire form (`Math.round(confidence * 1000)`) before the shared half-up
rule — replacing two mutually inconsistent web hand-rolls (`Math.round(* 100)` in the
badge, `.toFixed(0)` truncation in the queue) that had drifted from the native rule.

## Quota fraction

`fauna_core::format::quota_fraction(used_bytes: i64, max_bytes: i64) -> f64`: a
`{used_bytes, max_bytes}` usage pair (`fauna_protocol::account::UsageBytes`, the
`i64` wire form shared by `fauna.quota.get` / `fauna.account.get`) reduced to a
`0.0..=1.0` bar-fill fraction. Guarded against `max_bytes <= 0` (returns `0.0` —
no divide-by-zero) and a negative `used_bytes` (floors to `0`); clamped to `1.0`
so a row that has crept past its cap never overflows the bar past full.
`quota_percent(used_bytes, max_bytes) -> u32` is the whole-percent sibling,
rounded to the nearest percent (`(fraction * 100.0).round()`), for a text label
rather than a bar-fill value. Both are plain numbers, not `LocalizedText` — the
computation is locale-invariant; the surrounding "X / Y" byte-size label still
routes through [`byte_size`](#byte-sizes).

This unifies the per-app copies that each guarded the zero/over-quota edges
slightly differently — windows `total > 0 ? used / total * 100 : 0` (no
over-quota clamp, `SettingsViewModel.StoragePercent`), android `if (limit > 0)
used / limit else 0f` then `.coerceIn(0f, 1f)` (`AccountSettingsScreen`), apple
`guard max > 0 else { return 0 }; min(used / max, 1.0)` (`QuotaBar`), and web
`used_bytes / max_bytes` with **no** zero-guard (`+page.svelte` — a
`max_bytes == 0` quota row produced a `NaN` bar width, a real bug this closes)
— onto one source of truth (priority #1 minimize divergence / #2 shared Rust /
#4 resolve drift).

Exposed at the UniFFI boundary as `quotaFraction`/`quotaPercent`
(`libs/fauna-ffi/src/value_format.rs`, behind the default-on `value-format`
gate). **Windows adopted** (2026-07-08): `SettingsViewModel.StoragePercent` now
calls `FaunaFfiMethods.QuotaPercent` directly (no `ValueFormat` wrapper needed —
a plain number, mirroring `confidence_percent`'s direct-call convention); the
`SettingsGeneralPage` storage-bar text also stopped hand-rolling the MB
conversion, routing through `ValueFormat.ByteSize` instead. **Web adopted**
(2026-07-08) over the wasm boundary as `quotaFraction`/`quotaPercent`
(`libs/fauna-wasm/src/lib.rs` → `$lib/wasm` thunks): the `settings-storage-bar`
fill width now calls `quotaPercent`, fixing the `NaN`-on-zero-quota bug. **Apple
adopted** (2026-07-12): the shared `QuotaBar` over `quotaFraction` on macOS + iOS.
**Android adopted** (2026-07-15): `AccountSettingsScreen`'s storage bar + percent
text now call `quotaFraction`/`quotaPercent` directly (the `if (limit > 0) … / …
else 0f` then `.coerceIn(0f, 1f)` hand-roll + the `(progress * 100).toInt()`
percent both deleted). A second android call site, `StatusScreen`'s own
storage-quota bar, had the identical `if (limit > 0) … .coerceIn(0f, 1f)`
hand-roll independently — missed by the 2026-07-15 adoption pass since it's a
different screen; fixed 2026-07-23 (pure call-site swap onto the same already-bound
`quotaFraction`).

**Not byte-quota-specific — any `(count, total) -> 0.0..=1.0` bar-fill reuses it
directly, no new fn needed.** The mail-export wizard's progress bar
(`exported_count`/`total_count`, both `u32`) is the identical zero-guard +
clamp shape applied to message counts instead of usage bytes; linux
(`settings/mail_export.rs`) and android (`MailExportScreen.kt`) adopted
`quota_fraction`/`quotaFraction` directly 2026-07-22 (a pure call-site swap —
no shared-Rust/FFI/wasm change, the existing export already covers it), apple
joined 2026-07-23 (`MailExportView.swift`'s `progressFraction`), and tui
consumed it from its first build 2026-08-01 (`settings/mail_export.rs`) — so
it never had a hand-roll to unify. **windows adopted it 2026-08-25**
(`MailExportViewModel.cs`'s `ProgressFraction` now calls
`FaunaFfiMethods.QuotaFraction`, the same pure call-site swap) — the 7th and
last app to lift the wizard; no open leg remains.

## Port validation

`fauna_core::format::parse_port(input) -> Option<u16>`: parse a user-entered
port field — the CalDAV listener port and the nest serving port — into a valid
TCP port. (A third consumer, the WireGuard peer listen port, existed
2026-08-17 → 2026-08-23; see the case study below, which is kept for its
lesson, not its field.) Trims surrounding whitespace and returns `Some(port)` only for
`1..=65535`; `None` for empty, non-numeric, negative, fractional, `0` (not a
bindable listener port — `libs/fauna-protocol/src/wrapped_blob.rs` § CalDAV
listener port: "1–65535; 0 is rejected"), or above-range input. A leading `+` is
accepted (a valid unsigned literal — every app's native parser, C#
`ushort.TryParse` / Kotlin `toIntOrNull` / Swift `Int()`, already treats it as
valid). Unlike the display formatters above it returns a plain `Option<u16>`, not
a `LocalizedText`: this is *input validation*, not formatting — apps render
their existing `SERVING_PORT_INVALID` / `CALDAV_PORT_INVALID` i18n string (both
"Enter a port number between 1 and 65535.") on `None`.

This unifies the per-app range checks that each hand-rolled the same
`[1, 65535]` rule — windows `ushort.TryParse(s.Trim(), out p) || p < 1`
(`AdminCalendarViewModel` / `AdminNestViewModel`), android
`port == null || port < 1 || port > 65535` (`AdminCalendarScreen` /
`AdminNestScreen`), and apple's equivalent — onto one source of truth (priority
#1 minimize divergence / #2 shared Rust / #4 resolve drift). The canonical rule
(1–65535, reject 0) was already fixed in `wrapped_blob.rs` and the two i18n
strings; this is the shared *validator* behind those fields (the field specs live
in `docs/goal/ui/mail-settings.md` for CalDAV and the admin-nest config for
serving).

**Case study — the WireGuard peer listen port (2026-08-17 → deleted 2026-08-23).**
The field is gone with the WireGuard stack (owner
[`p2p.md`](p2p.md)), but it is the sharpest worked example this section has, so
it stays as a case study rather than being deleted with its subject. **Both
rules it produced are still binding on every port field.**

It was the only port field that was `Option<..>` on the wire — `None` meant "no
port, the nest decides", so an invalid draft correctly degraded to `None` rather
than raising a validation error (the field was labelled "Listen Port
(optional)"). It was a port all the same, so the `[1, 65535]` rule applied, and
the `0` case is what made it more than tidying: `0` parses fine as an integer,
so tui's bare `.parse::<i64>()` sent it — along with `-1` and `65536` — as a
peer's listen port. The fix moved the parse into `P2pState::listen_port`, a pure
helper precisely so a unit test could reach it; the inline parse it replaced sat
in an arm that needs a live nest, which is how it shipped unobserved.

**Rule 1 — an app clamp cannot answer for the nest.** The nest door
(`fauna.wireguard.peer.register`) took the same wire `Option<i64>` through a
bare `as u16`, which **wraps instead of refusing**: `-1` arrived as 65535,
`65536` and 2³² as the unbindable `0`, `70000` as 4464. The peer row then
carried an endpoint naming a port nobody listens on while the sender saw a
successful register — and a client older than the app-side clamp still put the
raw value on the wire. Every door that accepts a port validates the range
itself, no matter what the app already checked.

**Rule 2 — validate before the feature/service branch.** The door's check went
into a pure `validate_listen_port` helper called *before* the feature-gated
branch, so a runtime-less nest answered a bad request the same way a runtime-ful
one did (the `fauna.admin.gc` validate-before-the-service-lookup ordering). Both
rules were extracted for the reason the app-side entry above records: *a
validator inlined into an un-unit-testable arm is an unobserved validator* —
which is how this very field shipped unchecked on **both** sides.

Exposed at the UniFFI boundary as `parse_port`
(`libs/fauna-ffi/src/value_format.rs`, behind the `value-format` gate, returning
`Option<u16>`), consumed directly by android/apple/windows. Native Rust (linux,
tui) call `fauna_core::format::parse_port` directly (2026-07-23 — both
previously hand-rolled the identical `[1, 65535]` check inline instead of
calling the crate they already depend on). Also exposed via wasm as `parsePort`
(`libs/fauna-wasm/src/lib.rs`, returning `Option<f64>`); web's
`admin-calendar`/`admin-nest` `+page.svelte` consume it (2026-07-23 — web had
grown both client-side port fields, hand-rolling `Number.isInteger(port) &&
1 <= port <= 65535` locally, before this doc's "web does not validate a port
client-side today" note was corrected).

## Pagination

`fauna_core::format::total_pages(total, page_size) -> i64` / `current_page(offset,
page_size) -> i64`: the two-formula pair behind every admin list's
`"{current} / {total}"` page indicator (today: the admin-users hub,
`admin.md` § 2 Users). `total_pages` ceil-divides `total` items by `page_size`
and floors to `1` (an empty list still shows `"1 / 1"`, never `"1 / 0"` —
`((total + page_size - 1) / page_size).max(1)`); `current_page` is the 1-based
page number from a 0-based `offset` (`offset / page_size + 1`). Both are plain
`i64 -> i64`, not `LocalizedText` — the digits are locale-invariant, only the
`"{current} / {total}"` template (each app's `page_indicator` i18n key)
localizes.

This unifies the byte-for-byte-identical per-app formulas: web
(`admin/users/+page.svelte`, `PAGE_SIZE = 50`), windows
(`AdminUsersViewModel.CurrentPage`/`.TotalPages`, `PageSize = 50`), linux
(`views/admin.rs::USERS_PAGE_SIZE`), tui (`admin/users.rs`,
`USERS_PAGE_SIZE`) — all four hand-rolled the same ceil-div-and-floor and
offset-to-page-number math, three of them with the identical magic constant
`50` duplicated verbatim (priority #1/#2/#4). android doesn't implement
users-list pagination yet; apple's admin-users hub (shared FaunaKit
`AdminVM`) has prev/next pagination but no `"{current} / {total}"`
page-count display, so this display pair has nothing to lift there today
(distinct from the stepper below, which apple does consume).

Exposed at the UniFFI boundary as `total_pages`/`current_page`
(`libs/fauna-ffi/src/value_format.rs`) and via wasm as `totalPages`/
`currentPage` (`libs/fauna-wasm/src/lib.rs`, `i64` args/return cross as `f64`).
Landed 2026-07-23: web and linux and tui consume directly. **windows adopted
it 2026-08-25** (`AdminUsersViewModel.CurrentPage`/`.TotalPages` now call
`FaunaFfiMethods.CurrentPage`/`.TotalPages` — see § Implementation status
today) — no open leg remains on this pair.

**The next/prev stepper (added 2026-08-03) joins the same pair.**
`fauna_core::format::next_page_offset(offset, total, page_size) -> Option<i64>`
/ `prev_page_offset(offset, page_size) -> Option<i64>`: the offset one page
forward/back, or `None` at the last/first page — the
`(offset + page_size < total).then_some(offset + page_size)` /
`(offset > 0).then(|| (offset - page_size).max(0))` guards every admin-users
"next"/"prev" button hand-rolled identically (tui `admin/mod.rs`'s prior inline
guard, linux's next-click handler, web's `offset + PAGE_SIZE >= total` /
`offset - PAGE_SIZE` checks). `prev_page_offset` floors to `0` rather than
going negative, so a stale offset recovers instead of underflowing. This
supersedes the prior note in this section calling the stepper "too trivial on
its own to earn a shared fn" — the button-enabled *check* is a one-liner, but
the *next offset to fetch* is the same multi-operation formula pattern as
`total_pages`/`current_page`, and three apps had already hand-copied it
identically.

Exposed at the UniFFI boundary as `next_page_offset`/`prev_page_offset`
(`libs/fauna-ffi/src/value_format.rs`) and via wasm as `nextPageOffset`/
`prevPageOffset` (`libs/fauna-wasm/src/lib.rs`, `Option<i64>` crosses as
`number | undefined`). Landed 2026-08-03: tui (`admin/mod.rs::next_users_offset`/
`prev_users_offset`) and linux (`views/admin.rs`) call the core fns directly;
web consumes over wasm the same day (`admin/users/+page.svelte`). **windows
adopted it 2026-08-25** — `HasNextPage`/`HasPrevPage` now derive from the same
`NextPageOffset`/`PrevPageOffset` calls the stepper steps with (`is not null`),
rather than a separately hand-rolled comparison, so the enabled check and the
actual step can never disagree (`AdminUsersViewModel.cs:81-90,228-247` — see §
Implementation status today); android has no users-list pagination UI, so
there's nothing to lift there yet, same as the display pair above. apple
consumed the stepper 2026-08-22
(shared FaunaKit `AdminVM.swift::nextPage`/`prevPage` call `nextPageOffset`/
`prevPageOffset` directly; the hand-rolled `offset + pageSize < totalUsers`/
`max(0, offset - pageSize)` guards deleted). Unlike the display pair, this
guard logic already existed on apple (prev/next buttons since 2026-06-08,
missed by every prior sweep of this doc) and was
byte-for-byte identical to the shared fn — a pure dedup, not new UI.

## Nostr Connect (bunker) roster labels

`fauna_core::format::bunker_app_label(label, status) -> LocalizedText` /
`bunker_last_used_label(formatted_time: Option<&str>) -> LocalizedText`: the
two label decisions behind each `nostr-bunker-app-item` roster row
(`docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer). The bunker
flow transports no app name at connect time, so `bunker_app_label` returns
the app's own `label` verbatim if set, else `nostr.connected_apps.pending`
while the connection hasn't completed, else `nostr.connected_apps.unnamed`
(the non-empty-label arm reuses [`contact_status_label`]'s verbatim-passthrough
trick — an arbitrary label is very unlikely to collide with a real i18n key).
`bunker_last_used_label` is a plain `Option`-driven 2-key flip —
`nostr.connected_apps.never_used` when absent, else `nostr.connected_apps.last_used`
with the caller's own already-formatted time string as the `{time}` arg (the
formatting itself stays per-app, the same split every date/time field in
this doc uses — see § Absolute local timestamp display for why local-time
resolution can't move into pure Rust).

This unifies the byte-for-byte-identical roster-label logic hand-rolled
independently in web (`NostrSettingsSection.svelte`), linux
(`settings/nostr_tab.rs::{bunker_app_label,bunker_app_meta}`), and tui
(`nostr.rs::{bunker_app_label,bunker_app_meta}`) — all three the same
label-fallback branch and the same last-used/never-used flip, with linux and
tui's doc comments literally cross-referencing each other and web to stay in
sync by hand (the tell this belonged in shared Rust instead). windows/android/
apple have no Connected Apps UI built yet, so there's nothing to lift there
today.

Exposed via wasm as `bunkerAppLabel`/`bunkerLastUsedLabel`
(`libs/fauna-wasm/src/lib.rs`); web, linux, and tui consume directly. No
UniFFI export yet — no native-FFI app (windows/android/apple) has this UI,
so add one when the first of those builds it.

## Tier cap validation

`fauna_core::format::parse_cap(input) -> Option<i64>`: parse a user-entered admin
**tier-cap** field — the `admin-settings-tier-cap-{inbox,storage,devices,blob-size,feeds}`
inputs on the `admin-settings` page (`docs/goal/behavior/admin.md` § 3, in-place
tier-cap editing) — into a non-negative `i64`. Trims surrounding whitespace; a **negative**
value clamps to `0`, and a leading `+` is accepted (a valid signed-int literal
every app's native parser treats as valid). Returns `None` for empty,
non-numeric, fractional, or out-of-`i64`-range input. Like [`parse_port`] this is
*input validation*, not formatting, so it returns a plain `Option<i64>`, not a
`LocalizedText`. **Unlike** `parse_port`, `0` is a valid cap (an explicit "no
allowance"). Apps save as `parse_cap(text).unwrap_or(prev)`, so an
empty/unparseable edit keeps the persisted value and **never silently zeroes a
cap**; a negative is the one parseable case, clamped to `0` rather than dropped to
`prev`.

This unifies the per-app tier-cap parses that each hand-rolled the same
"non-negative `i64`, fall back to the persisted value, no silent zeroing" rule —
linux `entry.text().trim().parse::<i64>().unwrap_or(prev).max(0)`
(`views/admin.rs::parse_cap`), android `text.trim().toLongOrNull()?.coerceAtLeast(0)
?: prev` (`AdminSettingsVM.parseCap`), and windows `long.TryParse(s.Trim(), out v)
? Math.Max(0, v) : prev` (`AdminSettingsViewModel.ParseCap`) — onto one source of
truth (priority #1 minimize divergence / #2 shared Rust / #4 resolve drift). The
cap values are raw `i64` (bytes for the byte caps, counts otherwise; a unit-aware
editor is a future refinement per admin.md § 3).

**Two sanctioned consume shapes for the `None` case (ruled 2026-08-17).** The
`unwrap_or(prev)` shape above is what linux/android/windows/web do, and it
satisfies the invariant that actually matters — never silently zero a cap — but
it satisfies it *silently*: a bad edit reverts to the persisted value with no
feedback, so the admin cannot tell a rejected edit from a saved one. tui
instead refuses the whole save and paints `SAVE_TIER_ERROR_INVALID_CAP` on the
page's `error-message` (`admin/mod.rs::tier_update_req` returns `None`,
`apply_local` surfaces it). **That is the richer half and it stays** — per
priority #4, pick the richest existing pattern. Either shape is a valid
consume; a *third* shape that zeroes a cap, or one that drops the edit with no
feedback and no revert, is not. The negative clamp is **not** part of this
latitude: a negative is parseable, so it is a real edit and clamps to `0` under
both shapes. The remaining six apps adopting tui's error surfacing is a UI
question for `admin.md` § 3, not a validation one, and is not tracked as an open
leg here.

**The nest refuses a negative cap too, and the app clamp is not what makes that
unnecessary (2026-08-17).** `fauna.admin.tiers.{create,update}` answer
`fauna.admin.invalid_params` on any negative cap — contract owned by
[`../architecture/api-layers.md`](../architecture/api-layers.md) § Track C / C2,
which also records why. The short version: every clamp above landed 2026-08-17,
and an **older** app is a supported peer, so the door is the only surface that
can refuse what a pre-clamp build sends. `0` stays valid at the door exactly as
it is here — the refusal is `< 0`, never `< 1`.

Exposed at the UniFFI boundary as `parse_cap`
(`libs/fauna-ffi/src/value_format.rs`, behind the `value-format` gate, returning
`Option<i64>`). Native Rust (Linux) calls `fauna_core::format::parse_cap` directly.
The wasm `parseCap` boundary is added by the web adoption leg — web's
`admin/settings/+page.svelte` parses caps client-side today, so web is a real
consumer (unlike the port field).

## Mail-knob validation

`fauna_core::format::parse_count(input) -> Option<u32>` and its `u64` sibling
`parse_count_u64(input) -> Option<u64>`: parse a user-entered admin-**mail**
integer-knob field — the many `admin-mail-*-input` numeric fields across the
outbound / alias / spam / auth / submission / IMAP mail-policy sections
(`docs/goal/ui/mail-settings.md`) — into a non-negative integer. Trims surrounding
whitespace; a leading `+` is accepted (a valid unsigned literal every app's
native parser treats as valid). Returns `None` for empty, non-numeric, negative,
fractional, or out-of-range input. Like [`parse_port`] / [`parse_cap`] these are
*input validation*, not formatting, so they return a plain `Option<u32>` /
`Option<u64>`, not a `LocalizedText`. Apps save each knob as
`parse_count(text).unwrap_or(prev)` on the **full-PUT** mail-policy save (the page
starts from the persisted snapshot and overwrites each field), so an
empty/unparseable edit keeps the persisted value and **never silently zeroes a
knob**.

`parse_count_u64` exists for the single mail knob whose range can exceed `u32` —
the IMAP per-mailbox storage ceiling (`admin-mail-imap-storage-bytes-input` →
`storage_bytes_default`); every other mail integer knob (timeouts in hours/seconds,
rate-limit counts, score thresholds, `max_message_bytes`, …) is a `u32`.

This unifies the per-app mail-knob parses that each hand-rolled the same
"non-negative integer, fall back to the persisted value, no silent zeroing" rule —
linux `entry.text().trim().parse::<u32>().unwrap_or(prev)`
(`settings/admin_mail.rs::parse_u32` + `parse_u64`), windows
`uint.TryParse(text?.Trim(), out v) ? v : prev` (`AdminMailPage.ParseUint` +
`ParseUlong`), web `^\d+$` + `<= U32_MAX` then `Number(s)`
(`admin/mail/+page.svelte::parseU32` + `parseU64`), apple
`UInt32(s.trimmingCharacters(in: .whitespaces)) ?? prev` (`AdminMailView.parseU32` +
`parseU64`), and android `text.toUIntOrNull() ?: prev` (`AdminMailScreen` +
`toULongOrNull()`) — onto one source of truth (priority #1 minimize divergence / #2
shared Rust / #4 resolve drift). Each app applies the identical helper to
~15–18 knobs, so this is the highest-fan-out single validator in the family. The
lone edge-case drift converges here: web previously rejected a leading `+` (its
`^\d+$` guard); the canonical rule accepts it, matching `parse_cap` / `parse_port`.

`parse_count_i64(input) -> Option<i64>` is the signed sibling for the one **per-alias**
knob whose wire column is signed `i64`: the mail-alias `rate_limit_per_hour` override
(`mail-aliases-add-sheet-rate-per-hour-input` → `rate_limit_per_hour: Option<i64>`;
`mail-aliases.md` § Per-alias controls — null = unlimited, `0` = block all). The column is
signed only because SQLite/DAG-CBOR carry it that way; in *meaning* it is a non-negative
cap, so like every count-family member it yields a non-negative value — a **negative**
input (meaningless for a rate cap) is rejected to `None`, as are
empty/non-numeric/fractional/overflow. Unlike the admin-mail knobs (consumed
`.unwrap_or(prev)`), the optional add-sheet field consumes `None` as "no override /
unlimited" (not a fall-back-to-prev). This unifies the per-app per-alias override
parses — web `parseOptInt` (`Number(..)` + `Math.floor`: rejected a negative but accepted a
fractional), linux `parse_opt_i64` / windows `ParseOptI64` / apple `Int64(String)` / android
`toLongOrNull()` (all accepted a negative) — onto one rule that rejects **both** the
fractional (web's `Math.floor` drift) and the negative (the natives' incidental `long`-parse
drift; a negative cap reaching the nest would `451`-tempfail *all* alias mail). The sibling
`spam_threshold_override` (`Option<u32>`) needs no new fn — it reuses `parse_count`.

Exposed at the UniFFI boundary as `parse_count` / `parse_count_u64` / `parse_count_i64`
(`libs/fauna-ffi/src/value_format.rs`, behind the `value-format` gate). Native Rust
(Linux) calls `fauna_core::format::parse_count` / `parse_count_u64` / `parse_count_i64`
directly. The wasm `parseCount` / `parseCountU64` boundary is added by the web adoption leg
(the per-alias `parseCountI64` twin by its consume leg) — web parses every mail knob
client-side, so it is a real consumer (like `parseCap`).

## Tier rank

The subscription-tier form's **rank** field (`subscription-tier-form-rank`, the
profile Tiers tab's ordering integer — tier semantics stay with their owner;
this section owns only the parse) consumes the existing
[`parse_count`](#mail-knob-validation) — **no new fn** — as
`parse_count(text).unwrap_or(0)`. Rank is a plain non-negative ordering
integer and `0` is a valid rank (the bottom), so the consume floor is `0`, not
`prev`: the form buffer is seeded from the row on edit, so an emptied field is
a deliberate edit to the neutral floor, not an "unset" to preserve.

Ruled 2026-08-17 after the raw-operation grep found the six-app family split
5-vs-1: tui/linux hand-rolled `trim().parse::<u32>().unwrap_or(0)`, windows
`uint.TryParse(..) ? r : 0u`, apple `UInt32(..) ?? 0`, android
`toUIntOrNull() ?: 0u` — five behaviorally identical hand-rolls of the
canonical rule — while web's `parseInt(formRank.trim(), 10) || 0` drifted on
**two** axes, one of them a live bug: `parseInt` is lenient (`"5abc"` → rank 5)
and a **negative is truthy**, so `|| 0` never caught it and the wasm boundary's
`u32` ABI turned `-3` into `4294967293` — a tier silently outranking every
other. tui, linux and web consume the shared fn as of 2026-08-17; the
windows/apple/android hand-rolls are correct-as-written consume legs (via the
FFI `parse_count`, `libs/fauna-ffi/src/value_format.rs`) tracked in
§ Implementation status today.

## Factor weight

`fauna_core::format::parse_weight_permille(input) -> i64`: parse the create-feed
factor-weight editor's decimal **multiplier** (`feed-factor-weight-input`, e.g. `"2.0"`)
into the wire's **signed per-mille** `weight_permille`
(`fauna_feed::compose::FactorWeightInput` → `fauna_core::scoring::CompositionEntry`;
the composition model is owned by
[`../architecture/content-moderation-and-ranking.md`](../architecture/content-moderation-and-ranking.md)
§ Composition). Trims surrounding whitespace. Unlike the `parse_*` count family this
returns a **plain `i64`, not an `Option`**, because the editors pre-fill the `1.0`
baseline: an empty / non-numeric / non-finite entry falls back to `1000` rather than
silently dropping the user's Add. A **negative** weight is a designed case, not an
error — a strong-negative factor sinks an item below any rendered page, which is how
filtering falls out of ordering ([`../ui/feed.md`](../ui/feed.md) § Frame reconciliation).

Rounds **half-away-from-zero**, matching the sibling
`fauna_protocol::spam::probability_to_per_mille`, so no two apps disagree on a
midpoint. This is the whole reason the fn is shared: the three hand-rolls it replaces
disagreed on **two** independent axes. Linux
(`views/feed/feed_list.rs`) used `text.trim().parse::<f64>().unwrap_or(1.0)` then
`(w * 1000.0).round()` — strict parse, half-away-from-zero (the canonical rule). Web
(`routes/feed/+page.svelte`) used `Math.round(Number.parseFloat(t) * 1000)`, which drifts
twice: `parseFloat` is *lenient* (`"2abc"` → `2000`, where the canonical rule yields the
`1000` baseline), and JS `Math.round` is half-**up** toward +∞ (`-0.0025` → `-2`, where the
canonical rule yields `-3`). A C# hand-roll would have added a third axis — .NET's
`Math.Round` default is **banker's** (to-even) rounding. Priority #1 (minimize divergence) /
#2 (shared Rust) / #4 (resolve drift).

Exposed at the UniFFI boundary as `parse_weight_permille`
(`libs/fauna-ffi/src/value_format.rs`, behind the `value-format` gate) and at the wasm
boundary as `parseWeightPermille` (`libs/fauna-wasm/src/lib.rs`). Native Rust (Linux) calls
`fauna_core::format::parse_weight_permille` directly. Windows calls
`FaunaFfiMethods.ParseWeightPermille` **directly** — it returns a plain number, not a
`LocalizedText`, so it is *not* wrapped in `ValueFormat.cs` (the same rule
[`quota_percent`](#quota-fraction) follows).

`fauna_core::format::format_weight_permille(weight_permille: i64) -> String`: the
inverse — a wire `weight_permille` back to the create-feed factor chip's display
multiplier (`"{name} × {weight}"`, e.g. the factor list under
`feed-factor-weight-input`). Rounds to 2 decimal places and strips trailing zeros
(and a bare trailing `.`), so a whole multiplier reads `"1"`, never `"1.00"`.
Plain `String` (locale-invariant — a decimal multiplier, nothing to translate).

Unifies three independent per-app hand-rolls (found 2026-07-18, standing-watch
sweep): web `(w / 1000).toFixed(2).replace(/\.?0+$/, '')`, Windows `w / 1000.0`
formatted `"0.##"`, and Apple (macOS + iOS) `Double(w) / 1000` formatted `"%.1f"`.
Apple's fixed one-decimal form is the odd one out — it loses precision
`parse_weight_permille` itself preserves (`1234` → `"1.2"`, silently dropping the
last significant digit) — so the 2-decimal, trailing-zero-stripped shape
(web/Windows) is the richer convergent pattern kept here (priority #4: pick the
richest pattern, not the simplest). Linux's factor list (`views/feed/feed_list.rs`)
does not render the weight at all — only the factor name — so it has no hand-roll
to unify and gains no new call; whether the uniform shape should show the weight
there (and on Android, which has no create-feed factor editor yet) is a separate
product-surface question, out of scope for this lift.

Exposed at the UniFFI boundary as `format_weight_permille`
(`libs/fauna-ffi/src/value_format.rs`, behind the `value-format` gate) and at the
wasm boundary as `formatWeightPermille` (`libs/fauna-wasm/src/lib.rs`, taking
`f64` — `weight_permille` crosses as a JS `number`, not `bigint`). Web consumes
(`routes/feed/+page.svelte::factorLabel`); Windows/Apple's own hand-rolls are the
lift's target, entrusted per app NEXT for the consume leg.

## Backup destination status labels

The two live text rows under each `backup-destination-status-row`
([`backup-destinations.md`](backup-destinations.md) § Per-destination status read owns the
field spec and read shape; this section owns the text contract):

- `fauna_core::format::backup_last_upload_label(last_upload_secs, now_ms) ->
  BackupLastUploadDisplay { label, when }` — the
  `backup-destination-last-upload-time` text. `last_upload_secs` is the status's
  raw `last_upload_time` (unix **seconds**); `None` **or `0`** ⇒ `label` =
  `backups.backup_destination_last_upload_never` ("Last synced: never"),
  complete as-is, `when = None`. A real timestamp ⇒ `label` =
  `backups.backup_destination_last_upload` ("Last synced: {when}") plus `when =
  Some(RelativeTimeDisplay)` (§ Relative time; the seconds→ms conversion happens
  in shared Rust): the app resolves `when` first (the relative key through
  its i18n pipeline, or its native locale-aware date once ≥ 7 d old) and
  substitutes the result as the label's `{when}` arg. Two levels because a
  `LocalizedText` arg is a flat string — the inner relative time must be
  localized before substitution. ⚠ **The key and element ID say `upload`; the
  rendered copy says "Last synced" — deliberate, not drift.** The value is the
  last manifest POST, not a segment upload; only the *copy* was corrected
  (2026-07-29), because renaming the ID is a rule-A change the ruling declined.
  Do not "reconcile" the two: [`backup-destinations.md`](backup-destinations.md)
  § Per-destination status read owns the ruling.
- `fauna_core::format::backup_backlog_label(backlog_count) -> LocalizedText` —
  the `backup-destination-backlog-count` text
  (`backups.backup_destination_backlog`, "{count} queued"); `None` (no status
  read yet) carries the 0 baseline.

The never-vs-real decision, the epoch-`0` guard, and the seconds→ms conversion
live in shared Rust: the five hand-rolls this replaces had already drifted —
linux/web guarded `secs > 0` while apple/android would render a zero timestamp
as the 1970 epoch (priority #1/#4; the `> 0` guard is the richer shape).
Boundaries: UniFFI `backup_last_upload_label` / `backup_backlog_label`
(`libs/fauna-ffi/src/value_format.rs`, behind the `value-format` gate — off the
Go face, no Go binding churn); wasm `backupLastUploadLabel` /
`backupBacklogLabel` (`libs/fauna-wasm/src/lib.rs`). Linux and tui call the core
fns directly — the upload row through the shared `backup_last_upload_text` door
(§ Resolving a two-level display), the one-level backlog row through
`backup_backlog_label` + their own `resolve`.

### Audit labels (added 2026-07-29 with the first shell)

The same row gained a third line and the page a banner
([`../ui/backups.md`](../ui/backups.md) § Audit-alert surface owns the surface;
[`backup-restore.md`](backup-restore.md) § Background Tasks owns the loop):

- `backup_last_audit_label(last_passed_secs, now_ms) -> BackupLastAuditDisplay
  { label, when }` — the `backup-destination-last-audit-time` text. Structurally
  identical to the upload twin (never-vs-real, the epoch-`0` guard, seconds→ms,
  the two-level `{when}` resolve), and a **separate type on purpose**: the two
  rows answer different questions asserted by different parties — the upload row
  is the *source nest* reporting on its own work, this one is what the *client's
  own* audit independently confirmed. One type would invite rendering one where
  the other was meant, which is the confusion the audit exists to prevent.
  Keys: `backups.backup_destination_last_audit{,_never}`.
- `backup_self_audit_label(last_passed_secs, now_ms) -> BackupLastAuditDisplay`
  — the **same element on a client-device custodian row**, whose audit answer is
  the device's own (the owner-side loop has no address to reach it;
  [`backup-destinations.md`](backup-destinations.md) § Custodian contract,
  question 4). A third door rather than a flag on the two above, for the reason
  they are two: a flag is exactly how a caller renders one claim while meaning
  another, and here the two claims sit on opposite sides of the trust line. The
  wording names the provenance ("Self-checked: …"), and absence reads as *not
  yet* — never as a pass. Keys:
  `backups.backup_destination_last_self_audit{,_never}`.
- `backup_self_audit_is_alerting(audit_state) -> bool` — whether a custodian's
  reported verdict is one the page must be loud about. Only `AUDIT_STATE_FAILED`
  is; **absence and an unrecognised newer value are both quiet**, because a
  fleet-wide false data-loss alarm is the worst way to be wrong here. It is a
  shared predicate for the same reason `alert_reason()` is one: the loud/quiet
  decision must not be re-derived per app.
- `backup_audit_alert_label(reason, destination_label) -> LocalizedText` — the
  indexed `backup-audit-alert` banner, complete as-is (no client-side
  composition). `reason` is `BackupAuditAlertReason` (`Freshness { lag_secs }` /
  `Inclusion { missing, sampled }` / `Overdue { since_secs }` / `SelfReported`),
  the first three being the plain-data mirror of the three alerting verdicts —
  it lives here rather than in
  `fauna-client-backup` because that crate depends on `fauna-core`, not the
  reverse. `AuditVerdict::alert_reason()` is the single map between them, and
  `is_alerting()` is defined through it, so a verdict cannot alert without a
  banner. `SelfReported` is the exception that proves the arrangement: it has
  **no** verdict behind it, because the owner-side loop can never produce one for
  a custodian — it carries the device's own failing self-audit, arriving on the
  status row (added 2026-08-20, `../ui/backups.md` § Audit-alert surface → *The
  client-device arm*). Every reason names its destination (the banner is indexed,
  one per failing destination); durations render as whole days floored to ≥ 1,
  since both thresholds are multi-day and "3 days behind" is actionable where a
  relative timestamp is not.

Boundaries: linux and tui call the core fns directly — the audit row through the
shared `backup_last_audit_text` door (§ Resolving a two-level display), the
complete-as-is banner through `backup_audit_alert_label` + each app's own
`resolve`; wasm `backupLastAuditLabel` / `backupAuditAlertLabel`
(`libs/fauna-wasm/src/lib.rs`), consumed by
`$lib/value-format::{backupLastAuditText,backupAuditAlertText}` since 2026-07-30.
No UniFFI face yet — the four native shells that would need one have no audit
surface at all, and their prerequisite is the whole pass's FFI design rather than
these two labels (`../ui/backups.md` § Audit-alert surface → *Implementation
status*). ⚠ On the wasm boundary `reason` crosses **opaquely**: it comes out of a
`backupAuditRunPass` row and goes straight back into `backupAuditAlertLabel`, so the
SPA neither inspects nor constructs a `BackupAuditAlertReason` — keeping the
loud/quiet decision unreachable from JS, which is the point of routing it through
`alert_reason()` at all.

### Client-device destination labels (added 2026-08-03 with the third destination kind)

The client-device custodian kind ([`backup-destinations.md`](backup-destinations.md) § Third
destination kind owns the surface and the ratified shape; this section owns the
text contract) adds four faces to the same page, plus one predicate that is not a
label at all — and, since the reclaim affordance landed 2026-08-21, one more
face and one more predicate for the orphaned-store row:

- `backup_destination_kind_label(kind) -> LocalizedText` — the
  `backup-destination-kind-badge` text
  (`backups.backup_destination_kind_{nest,client_device,unknown}`). Takes the raw
  discriminator rather than the typed `DestinationKind` projection, because the
  badge answers only *which kind is this*. ⚠ **An unrecognised kind renders as
  itself** — the raw string interpolated into the `_unknown` key, never collapsed
  into a generic word: a row a newer client wrote is precisely the case where the
  user needs to see *what* their older build cannot drive.
- `backup_destination_kind_options() -> Vec<BackupDestinationKindOption
  { value, label }>` — the `backup-destination-kind-select` catalog, implemented
  kinds in paint order (nest first: it is the kind that actually satisfies
  "off-site"). Shared for one reason specific to a select: **the option a user
  picks and the badge they get back must be the same text**, so an app must not
  pair a hand-written option list against the label fn above. The
  ratified-but-deferred S3 kind is *absent*, not present-and-disabled.
- `backup_usage_label(held_bytes, capacity_cap_bytes, cap_state) ->
  BackupUsageDisplay { label, held, cap }` — the `backup-destination-usage` text,
  client-device rows only. Split two-level for the same reason the upload twin is
  (a `LocalizedText` arg is a flat string, so the inner byte sizes localize
  first). ⚠ **Cap-reached is READ from `cap_state`, never inferred from
  `held >= cap`**: a pull pass that stops at its cap ends *below* the cap (a
  segment larger than the remaining headroom stops the pass without filling it),
  so an app re-deriving the verdict renders "healthy, with room to spare" for a
  backup that has silently stopped advancing. `held_bytes: None` is "never
  checked in" — *nothing held yet*, which is a different claim from *0 bytes
  held*. Keys: `backups.backup_destination_usage{,_cap_reached,_uncapped,_unknown}`.
- `parse_byte_size(input) -> Option<u64>` — the inverse of `byte_size` and the
  read behind `backup-destination-capacity-input`. Units are **1024-based**,
  matching `byte_size`'s own scaling, so a cap round-trips through the pair
  unchanged instead of drifting every repaint. `None` is a **refusal the shell
  surfaces**, never a substituted default — silently recording a cap the user did
  not choose is the class of guess that fills a device's disk.
- `fauna_core::data::every_destination_is_a_client_device(rows)` /
  `every_row_is_a_client_device(rows)` — the
  `backup-sole-client-destination-warning` predicate. **A policy answer, not a
  rendering one**, which is why it is shared: it decides whether a user is told
  their durability story is weaker than they think. Both arms are the
  conservative direction — an empty list is *not* sole-client (painting a
  durability warning on an account with no backup at all is false), and a row
  whose kind this build does not implement counts as *not* a client device (it
  may well BE the off-site copy the warning would otherwise deny the user has,
  and crying wolf at someone who is covered is how a standing warning gets tuned
  out).

- `orphaned_store_label(held_bytes) -> OrphanedStoreDisplay` / `orphaned_store_text(..)`
  (added 2026-08-21 with the reclaim affordance) — the
  `backup-orphaned-store-row` sentence, two-level like `backup_usage_label`
  because the byte size is itself a `LocalizedText` (unit key + value), with the
  same `_text` resolve door for the shells that would otherwise hand-roll it.
  Shared for the reason the rest of this family is: the row is the only place a
  user is told that freeing these bytes costs them their standalone restore, and
  seven apps writing that sentence is seven chances to undersell it. **The
  formatter does not decide visibility** — that is
  `custodian_store_is_orphaned` below; a formatter that also gated the row would
  be a policy answer wearing a label's clothes. ⚠ **Neither this face nor the
  predicate below crosses a boundary yet** — tui links `fauna-core` directly, so
  its lead leg needed no export, and adding a `uniffi::Record` to `fauna_core`
  reaches the Go mail-bridge binding tree *regardless* of which feature gates the
  functions returning it (the `BackupDestinationKindOption` lesson,
  [`backup-destinations.md`](backup-destinations.md) § Implementation status
  today). The UniFFI + wasm exports are owed with the first non-linking app's
  render leg, exactly as the five faces above were.
- `fauna_core::data::custodian_store_is_orphaned(rows, this_device_id, store_holds_bytes)` /
  `a_destination_row_claims_this_device(rows, this_device_id)` (added 2026-08-21) —
  the `backup-orphaned-store-row` render rule and its claim half. A policy
  answer like the predicate above, and shared for a sharper reason: it guards a
  **destructive** gesture. ⚠ **It is not
  `custodian_assignment_for(..).is_none()`** — that function answers *"can this
  device host?"* and refuses (`None`) when two rows name this device, because a
  host that guessed which cap to honour is worse than one that stops; this one
  answers *"is this store still somebody's custody?"*, where two rows naming it
  is emphatically yes. Reading the refusal as an absence would offer to delete
  the owner's only offline copy while both rows sit on their own Backups page.
  Both refusals are the conservative direction: a device with no sync id of its
  own is never orphaned (cannot-tell must not paint a delete button), and an
  empty store has nothing to free.


Boundaries (all five landed 2026-08-03): linux and tui call the core fns
directly — the two-level usage row through the shared `backup_usage_text` door
(§ Resolving a two-level display), the rest through their own `resolve`; UniFFI
`backup_destination_kind_label` / `backup_destination_kind_options`
/ `backup_usage_label` / `parse_byte_size` (`libs/fauna-ffi/src/value_format.rs`,
behind the `value-format` gate) + `every_destination_is_a_client_device`
(`libs/fauna-ffi/src/backup_destinations.rs`, behind `backup-destinations`) —
both gates are off the Go mail-bridge `--no-default-features` build, so the
tracked Go binding tree is unchanged; wasm `backupDestinationKindLabel` /
`backupDestinationKindOptions` / `backupUsageLabel` / `parseByteSize` /
`everyDestinationIsAClientDevice` (`libs/fauna-wasm/src/lib.rs`), wrapped for TS in
`$lib/wasm`.

A sixth face followed on the **UniFFI side only** (2026-08-03, with android's
leg): `destination_kind_client_device() -> String`, the `client-device` wire
discriminator itself. A **select** asks a question no other face answers — *does
the kind the user just picked mean this device?* — which is a pure discriminator
comparison with no row to project yet, so neither `every_destination_is_a_client_device`
(typed, needs a whole row) nor the option catalog (carries values, not their
meaning) can be asked it. Without the accessor a native shell must hard-code
`"client-device"`, minting exactly the private mirror of a shared constant that
`default_lapse_tier()` exists to delete elsewhere — and drifting it is not
cosmetic: the shell paints the URL box for a kind with no address and submits
through the nest path, which resolves an empty URL over the network. Pinned
against the catalog by `value_format.rs::the_exported_client_device_discriminator_names_a_real_catalog_option`,
so the accessor and the options cannot name different strings. Linux and tui need
none (they name the constant); **web needs none either** — its Backups page
renders these rows but offers no kind select at all, so it never holds a selected
kind (see [`backup-destinations.md`](backup-destinations.md) § Implementation status today).

⚠ **`backup_usage_label`'s third argument had no carrier until 2026-08-03.**
`held_bytes` + `cap_state` reach a client only through the nest's
`fauna.backup.status` projection, and **both boundary status records dropped
them** — `FfiBackupDestinationStatus` (`libs/fauna-ffi/src/segment_backup.rs`) and
wasm's `WasmBackupDestinationStatus` (`libs/fauna-wasm/src/rpc.rs`) each carried
only `destination_id` / `last_upload_time` / `backlog_count`, so the label's
own cap-reached rule was unreachable from android, web, windows or apple, and the
only answer left to those shells was the `held >= cap` inference the rule exists
to forbid. Both records now carry the pair (the wasm twin's TS interface with
them; the FFI one pinned by `backup_destinations.rs::the_status_projection_carries_held_bytes_and_cap_state`,
which asserts a *below-cap yet cap-reached* row survives — the exact pair an
inference gets wrong). Same class as the `FfiBackupDestinationView` widening
above: a shared rule is only as good as the narrowest projection between it and
the shell.

⚠ **The predicate's row shape is "the destination list your own list call
returned", not a new record** — and the two boundaries therefore carry it
differently *by construction*, which is the honest uniform answer rather than a
divergence: wasm's `backupDestinationList` already serializes the whole
`BackupDestination`, while UniFFI hands back the `FfiBackupDestinationView`
read projection, which **gained `kind` / `custodian_device_id` /
`capacity_cap_bytes` in the same change**. That widening was owed regardless of
the predicate: without `kind` on the projection no native shell can paint the
badge or decide whether to paint the usage row at all. Underneath, one rule —
`fauna_core::data::row_is_a_client_device` over the carrier-agnostic
`CustodianRowRef` — which `every_destination_is_a_client_device` delegates to and
which `backup_destination_kind.rs::row_predicate_agrees_with_kind_view` pins
against `kind_view()`, so the two expressions cannot drift.

Boundaries: linux calls the core fns directly via
`crate::i18n::{backup_last_audit, backup_audit_alert}`. No UniFFI/wasm mirror yet
— it lands with the first FFI/web shell, so the surface arrives with a consumer.

## DNS verdict label

`fauna_core::format::dns_verdict_label(status, observed) -> LocalizedText` — the
per-record red/green verdict text on `admin-dns` (`../ui/admin.md` owns the row
spec; this section owns the text contract). `Ok` → `admin.dns.status_ok`,
`Missing` → `status_missing`, `Mismatch` → `status_mismatch_found`
(`"Mismatch — found {found}"`, `found` = the observed values joined with `", "`);
**anything else** — `Checking`, an absent verdict (`""`), or a wire/version-drift
value — → `status_checking`. Takes the `VerifyStatus` serde variant name as a
plain string, not the typed enum, because `fauna_core` does not depend on
`fauna-protocol`.

**A red verdict must say what public DNS actually served** (added 2026-07-29).
The nest already resolves every expected record and puts the observed values on
the wire (`RecordVerdict.observed`); before this they reached the apps and
none rendered them, so a `Mismatch` was a dead-end — it could not answer
"mismatch with *what*", which is exactly the question the 2026-07-29 live
walkthrough got stuck on. `observed` is reported **only where it helps**:
`Missing` is `observed.is_empty()` by construction
(`fauna_mail::dns::verify::compare_*`) and a green row needs no forensics, so
both keep their bare labels and empty `args`. A `Mismatch` carrying no observed
values — reachable only from a nest too old to send them — degrades to the plain
`status_mismatch`, so the label stays correct across a version skew.

The verdict's **colour/CSS class** (`ok`/`bad`/`checking`) stays an idiomatic
per-app render — the class *names* are platform vocabulary (a CSS class, a
`Brush` key, a Compose colour), so only the verdict→text decision is shared.

## Cert status badge

The `admin-dns-cert-status` served-cert health badge
([`../architecture/nest/tls-certificates.md`](../architecture/nest/tls-certificates.md)
§ C.4 owns the row's existence and its three states; § Where logic lives there is
explicit that **per-app shells "render the cert-status row … no cert logic of
their own"** — this section owns the text contract that makes that true):

- `fauna_core::format::cert_status_label(state) -> LocalizedText` — the **state
  word** alone. `ValidTrusted` → `admin.dns.cert.status_valid`, `Expiring` →
  `status_expiring`; **anything else** — `OnFloorRenewNeeded` (the fresh-nest
  default) or a drift value — → `status_on_floor` ("Renew needed"). The safe
  fallback is deliberate: an unrecognized state must never read as healthier than
  it is. Serde variant name as a plain string, per the `dns_verdict_label` rule
  above.
- `fauna_core::format::cert_status_view(state, is_floor, not_after_unix) ->
  CertStatusView { state, show_self_signed, expires_at_unix }` — **the whole
  badge, and what an app should call.** `state` is `cert_status_label`'s key;
  `show_self_signed` and `expires_at_unix` are the two **mutually-exclusive**
  sub-labels. Apps render `"{admin.dns.cert.label} {state}"`, then append
  `({admin.dns.cert.self_signed})` when `show_self_signed`, or
  `admin.dns.cert.expires` + `{date}` when `expires_at_unix` is `Some` —
  formatting that epoch with the platform's own locale-aware date formatter (the
  same decision-here/date-there split as § Relative time's
  `RelativeTimeDisplay`). `expires_at_unix` is `None` for the floor, and for the
  `0` a nest with no TLS resolver at all reports.

**A floor cert's expiry is withheld, and this is the whole point.** The
self-signed floor carries a *genuine, far-future* `not_after` (the nest mints it
with no override, so the cert library's 4096-01-01 default applies) — so an app
that tests `is_floor` and `not_after_unix > 0` **independently** renders
`Certificate: Renew needed (self-signed) — expires 4096-01-01` on every
TLS-serving fresh nest: a two-millennium expiry reading as *reassurance* on an
untrusted cert. What the admin needs there is a trusted cert, not the floor's own
lifetime. Four apps had this right as an `else if` chain and apple did not
(priority #1/#4) — apple's copy has since been corrected in place, but *that a
correct copy exists on all six is not the same as the rule having one home*,
which is exactly why the exclusion is now structural rather than a shape six
hand-rolls must each preserve forever. The unit fixtures that missed it used
`not_after_unix: 0`.

**Why a struct, not one `LocalizedText`.** A `LocalizedText`'s `args` are flat
strings, so it cannot carry a *translatable* sub-label nested inside it (the same
constraint § Provisioning step display records), and the expiry must stay a
client-side locale-aware date. Keying every combination instead — three states ×
floor × expiry-present — does not scale. So the shared decision is a small view
struct and the *assembly* stays client-side text concatenation. This supersedes
the earlier note in `cert_status_label` that the composition, self-signed
sub-label, and expiry date "stay a per-app interpolation": that reason argued
against one flat `LocalizedText`, which is an argument **for** a struct view, not
against sharing.

The badge's **colour** stays an idiomatic per-app render, the same split as
`dns_verdict_label` above.

Boundaries: UniFFI `cert_status_label` / `cert_status_view` / `dns_verdict_label`
(`libs/fauna-ffi/src/value_format.rs`, behind the `value-format` gate — a
`LocalizedText`-carrying export must stay gated, so it is off the Go face); wasm
`certStatusLabel` / `certStatusView` / `dnsVerdictLabel`
(`libs/fauna-wasm/src/lib.rs`). Linux calls the core fns directly.

## Implementation status today

**Every contract above is landed in shared Rust (§ Peer display label the latest, 2026-09-26, with the private contact overlay's shared-Rust slice)** (`fauna_core::format` / `fauna_provisioning::progress`), each with lib unit tests pinning the bucketing/rounding/parse rules and with UniFFI (`libs/fauna-ffi/src/value_format.rs`, `identity.rs`) + wasm boundaries where a consumer exists. Adoption matrix (✅ = consumes the shared fn; ⏳ = open consume leg; — = no consumer surface on that app yet, consume-from-start when it gains one):

| Formatter | web | linux | windows | macos | ios | android | tui |
|---|---|---|---|---|---|---|---|
| Relative time / byte size / duration | ✅ 2026-06-01 | ✅ | ✅ | ✅ 2026-06-06 | ✅ 2026-06-06 (no uptime UI — conformance-only for duration) | ✅ 2026-06-03 | ✅ (`format.rs`/`search.rs` call `relative_time`/`byte_size` directly; no uptime UI, same conformance-only caveat as macos/ios) |
| Conversation timestamp (+ per-bubble) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (all 6, 2026-06-23) | ✅ (bubbles `message_timestamp_text`; the list row `conversation-item-timestamp` 2026-09-26, both through `conversation_timestamp_display`) |
| Provisioning elapsed | ✅ | ✅ | ✅ | — | — | ✅ (2026-06-03) | ✅ (`wizard/nest_provisioning.rs` calls `progress::elapsed_display` directly) |
| Provisioning step display | ✅ | ✅ 2026-06-15 | ✅ | ✅ | ✅ | ✅ 2026-06-14 | ✅ (`wizard/nest_provisioning.rs` calls `progress::status_glyph`/`step_label`/`substep_label` directly) |
| Short id | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (Phase 2 complete all six) | ✅ (`admin/users.rs:86,332`) |
| Fleet fingerprint (rendered in `DevicesSnapshot`, so a leg reads a string) | ✅ 2026-09-30 (`DevicesSection.svelte`'s member group + the own row, off the snapshot the account port fills — `../architecture/account-client-lifecycle.md` § The client-side lifecycle → *The account port*) | ⏳ (owes the member-group render) | ⏳ | ✅ 2026-09-25 (shared FaunaKit `DevicesContent`'s `MemberCard` + `DeviceCard`'s own-fingerprint badge) | ✅ 2026-09-25 (same shared FaunaKit renderer as macos) | ⏳ | ✅ 2026-09-25 (`settings/devices.rs::member_elements` + the own row) |
| Short nest id | ✅ 2026-07-09 | ✅ (direct call, `views/onboarding/nest_recovery.rs`; verified 2026-07-10) | ✅ 2026-07-06 | ✅ 2026-07-31 (`NestRecoveryView.swift:104`, shared FaunaKit box-recovery Task E — calls `shortNestId(id:)`) | ✅ 2026-07-31 (same shared FaunaKit view as macos) | ✅ 2026-07-17 (`NestRecoveryScreen.kt`; fixes real drift — the local helper was `short_id`'s 12-char-prefix shape, not `short_nest_id`'s first-8+…+last-8 shape, despite its comment claiming to mirror linux) | ✅ (direct call, `wizard/nest_recovery.rs:59`) |
| Account display label | ✅ 2026-07-09 (a visible fix: empty-handle fallback + ellipsis) | ✅ (`settings/account.rs`; verified 2026-07-10) | ✅ 2026-07-19 (`AccountSwitcherViewModel.cs:114` + `App.xaml.cs`, multi-account switcher — consumed `FaunaFfiMethods.AccountDisplayLabel` from the start of the build, as this row anticipated) | ✅ 2026-07-14/15 (`AccountSwitcherVM.swift`, apple multi-account switcher) | ✅ 2026-07-14/15 (shared FaunaKit switcher, macOS+iOS) | ✅ 2026-07-18 (`AccountSettingsVM.kt::accountLabel` calls `accountDisplayLabel` directly, multi-user Stage-1 switcher — consumed from the start of the build) | ✅ (`session.rs:746`, the account-switcher list's row prefix) |
| Peer display label (`peer_display_label`) | ⏳ (web hosts the account runtime since 2026-09-29, so its leg is the full lift — no `nickname = None` interim; not yet built) | ✅ 2026-10-01 (through the projection's reads, `fauna_conversations::ContactsCache::{peer_label, subscription_author_label}`: `views/contacts/list.rs` roster row + knock sender, `views/feed/post_list.rs::author_label_text` card + detail with the bridged face, `views/profile/mod.rs::HeaderNames` OTHER header, `settings/subscriptions.rs` subscription author label; message bubbles and member chips from the shared conversations snapshot) | ⏳ | ✅ 2026-10-03 (through the UniFFI face `FfiContactOverlays`, read off `ConversationsVM.contactOverlays`: shared FaunaKit `ContactsVM.rosterGroups` roster row, `ConversationsVM.postAuthorLabel` feed card with the bridged face, `ProfileView` OTHER header, `SubscriptionSettingsView` subscription author label; per target `MacContactListView` roster row + knock sender and `MacFeedDetailView` detail author; message bubbles and member chips from the shared conversations snapshot) | ✅ 2026-10-03 (same shared FaunaKit leg as macos; per target `ContactsView` roster row + knock sender and `PostDetailView` detail author) | ✅ 2026-10-03 (through the UniFFI face `FfiContactOverlays`: `ContactsScreen.kt` roster row + knock sender, `FeedScreen.kt::postAuthorLabel` card + detail with the bridged face, `ProfileScreen.kt` OTHER header, `SubscriptionSettingsScreen.kt` subscription author label; message bubbles and member chips from the shared conversations snapshot) | ✅ 2026-09-26 (`contacts.rs::contact_display` roster row + knock sender, `feed/mod.rs::author_label` card + detail, `profile/mod.rs::header_label` OTHER header, `settings/subscriptions.rs` subscription author label over the § Subscription author label chooser as its public name; member chips from the shared snapshot's `participant_displays`) |
| Pending-share sharer label (`shared_by_display`, pre-computed in `fauna-client-inbox`) | ✅ 2026-07-22 (`FoldersSection.svelte::pendingShareSharer` reads `share.sharedByDisplay`) | ✅ 2026-07-14 (`folders.rs::build_pending_share_row`; local 12-char truncation deleted) | ✅ 2026-07-14 (`FoldersPage.xaml.cs::BuildPendingShareRow`, `common/unknown` as the empty-string fallback) | ✅ 2026-07-16 (`FolderPendingShareRow.who`; local handle-else-`prefix(12)` deleted, empty → `L.common.unknown`) | ✅ 2026-07-16 (same shared FaunaKit row as macos) | ✅ 2026-07-16 (`PendingShareRow`, `FfiPendingShare.sharedByDisplay`, empty → `common_unknown`) | — (folder sharing not yet built in tui) |
| Owner-side folder badge (`FolderSummary.owner_display`, pre-computed in the `fauna-devices-machine` transcribe) | ✅ 2026-07-22 (`FoldersSection.svelte:778`, `fs.owner_display`) | ✅ 2026-07-15 (`folders.rs::build_member_folder_row`; local `owner_handle`-with-ellipsis fallback deleted) | ✅ 2026-07-15 (`FoldersPage.xaml.cs`, `fs.ownerDisplay`) | ✅ 2026-07-16 (`FoldersContent.memberFolderRow`; local `sharedByWho` handle-else-`…` fallback deleted) | ✅ 2026-07-16 (same shared FaunaKit renderer as macos) | ✅ 2026-07-16 (`MemberFolderRow`, reads `FolderSummary.ownerDisplay` directly, no local truncation) | — (folder sharing not yet built in tui) |
| Subscription author label (`author_display_label`, pre-computed as `author_display` at the Ffi mirror + the wasm row JSON) | ✅ 2026-07-16 (`SubscriptionsSection.svelte` reads `sub.author_display`; the local `authorLabel` deleted) | ✅ 2026-07-16 (`settings/subscriptions.rs::build_row` direct call; the local `filter(!is_empty)`-else-`hex_full` chain deleted) | ✅ 2026-07-18 (`SubscriptionsSettingsViewModel.cs:48` reads `s.authorDisplay` directly; the local `handle is { Length: > 0 } h ? h : Hex(...)` ternary + the now-dead `Hex` helper deleted) | ✅ 2026-07-18 (`SubscriptionSettingsView.swift` reads `sub.authorDisplay` directly; the local `authorLabel` helper deleted) | ✅ 2026-07-18 (same shared FaunaKit view as macos) | ✅ 2026-07-17 (`SubscriptionSettingsScreen.kt::MineRow` reads `sub.authorDisplay` directly; the injected `hexFull` stub deleted from `MineRow`/`SubscriptionSettingsContent`) | — (subscriptions settings sub-page not yet built in tui) |
| Hex id display (`hex_short`/`hex_full`) | ✅ | ✅ | ✅ 2026-07-01 | ✅ 2026-07-02 | ✅ 2026-07-02 | ✅ | — (no raw-hex fallback display site yet) |
| URL host display (`url_host`) | ✅ 2026-07-24 (`urlHost` wasm export; `LinkPreviewCard.svelte` — was hand-rolled `new URL(u).hostname`) | ✅ (direct call, `main.rs`/`views/document.rs`) | ✅ (`FaunaFfiMethods.UrlHost`) | ✅ (shared `ValueFormat.urlHost` wrapping `FaunaFFISwift.urlHost`) | ✅ (same shared `ValueFormat.urlHost`) | ✅ (`com.fauna.ffi.urlHost`) | ✅ (direct call, `feed/mod.rs:911`) |
| Confidence percent | ✅ | ✅ | ✅ 2026-07-02 | ✅ | ✅ | ✅ | ✅ 2026-07-29 (`apps/fauna-tui/src/moderation.rs:306` calls `confidence_percent` directly, queue landed with tui's Moderation page) |
| Quota fraction/percent | ✅ | ✅ | ✅ | ✅ 2026-07-12 (`QuotaBar.fraction`; was hand-rolled until this date — the row's prior bare ✅ pre-dated the actual apple consume) | ✅ 2026-07-12 (shared `QuotaBar`) | ✅ 2026-07-15 (`AccountSettingsScreen`; was hand-rolled until this date — the prior bare ✅ pre-dated the actual android consume) | ✅ 2026-08-01 (`settings/mail_export.rs` calls `quota_fraction` directly for the export wizard's progress bar, painted as a percentage + a `fraction` attr — consumed from first build, never hand-rolled. The *storage-quota* row remains `used / max` text via `byte_size`, `settings/mod.rs::QuotaView`: a terminal has no bar-fill widget, which is why this cell read `n/a` until a count-based bar existed) |
| Port validation | ✅ 2026-07-23 (`parsePort` wasm export; `admin-calendar`/`admin-nest` `+page.svelte` — was hand-rolled `Number.isInteger(..)` + range check) | ✅ 2026-07-23 (direct call, `views/admin.rs`/`settings/admin_calendar.rs` — was hand-rolled `.trim().parse::<u16>()` + `port >= 1`) | ✅ | ✅ 2026-06-28 | ✅ | ✅ 2026-06-27 | ✅ 2026-07-23 (direct call, `admin/mod.rs:959,1014` — was hand-rolled `.trim().parse::<u16>()` + `port >= 1`) |
| Tier cap validation | ✅ | ✅ | ✅ | ✅ 2026-06-28 | ✅ | ✅ (all, 2026-06-27/28) | ✅ 2026-08-17 (`admin/mod.rs::tier_update_req` calls `parse_cap` directly — the missing negative clamp was a live bug, sending a negative allowance on a row that looked valid. Its `?`-chain **deliberately keeps** refusing the whole save + painting `SAVE_TIER_ERROR_INVALID_CAP`, rather than adopting `unwrap_or(prev)` — see the consume-shape note in § Tier cap validation) |
| Mail-knob validation (`parse_count`/`_u64`) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (all, 2026-06-28) | ✅ (`admin/mail.rs` calls `parse_count`/`parse_count_u64` directly) |
| Tier rank (`parse_count` reuse — § Tier rank) | ✅ 2026-08-17 (`profile/[[actorId]]/+page.svelte::saveForm` calls `parseCount`; the `parseInt(..) \|\| 0` hand-roll was a live bug — a negative is truthy, and the wasm `u32` boundary turned `-3` into `4294967293`, a tier outranking every other) | ✅ 2026-08-17 (`views/profile/tiers.rs::submit_form` direct call; hand-roll deleted) | ⏳ (`ProfileViewModel.cs:307` `uint.TryParse(..) ? r : 0u` — behaviorally identical, consume via `FaunaFfiMethods.ParseCount`) | ⏳ (`SubscriptionsVM.swift:213` `UInt32(..) ?? 0` — behaviorally identical) | ⏳ (same shared FaunaKit VM as macos) | ✅ 2026-08-21 (`ProfileTiersTab.kt` — `toUIntOrNull() ?: 0u` swapped for an injected `parseRank` defaulting to `com.fauna.ffi.parseCount`, the same FFI-free-injection idiom the file's `hexFull`/`claimStatusLabel`/`providerStatusLabel` already use. **Behaviour-preserving, and the reason is worth keeping**: the two parsers differ only on surrounding whitespace, and the rank field's `onValueChange` strips every non-digit as it is typed, so no reachable input distinguishes them — unlike web's and tui's legs, android's was pure uniformity, not a latent bug. That also makes a value-difference test *vacuous*, so the pin is a sentinel-parser injection (`ProfileTiersContentTest.tierForm_routesTheRankThroughTheInjectedSharedParser`) that the old call site fails by ignoring the injection outright — **red-verified, not asserted**: restoring `rank.toUIntOrNull()` reds exactly that one case of the class's 22 with `expected:<4242> but was:<7>`, and the whole class is 22/22 green with the swap in place) | ✅ 2026-08-17 (`profile/mod.rs` `Op::SaveTier` direct call; hand-roll deleted) |
| Pagination (`total_pages`/`current_page`) | ✅ 2026-07-23 (`admin/users/+page.svelte` — was hand-rolled `Math.ceil`/`Math.floor`) | ✅ 2026-07-23 (direct call, `views/admin.rs` — was hand-rolled ceil-div) | ✅ 2026-08-25 (`AdminUsersViewModel.CurrentPage`/`.TotalPages` call `FaunaFfiMethods.CurrentPage`/`.TotalPages`; the hand-rolled formulas deleted) | — (has prev/next pagination but no `"{current} / {total}"` page-count display — nothing to lift for this pair) | — (same, shared FaunaKit `AdminVM`) | ✅ 2026-08-22 (`AdminUsersScreen.kt`'s `UsersSection` calls `com.fauna.ffi.{totalPages,currentPage}` for the new `"Page {current} of {pages}"` indicator, injected FFI-free per the file's `hexFull`/`mailServingStatusLabel` idiom — android is the first app to render this pair for the users list) | ✅ 2026-07-23 (direct call, `admin/users.rs` — was hand-rolled ceil-div) |
| Pagination stepper (`next_page_offset`/`prev_page_offset`) | ✅ 2026-08-03 (`admin/users/+page.svelte` calls `nextPageOffset`/`prevPageOffset`; the hand-rolled `offset + PAGE_SIZE >= total`/`offset - PAGE_SIZE` guards deleted) | ✅ 2026-08-03 (direct call, `views/admin.rs`) | ✅ 2026-08-25 (`AdminUsersViewModel.{NextPageAsync,PrevPageAsync}` call `FaunaFfiMethods.{NextPageOffset,PrevPageOffset}`; `HasNextPage`/`HasPrevPage` now derive from the same calls (`is not null`) rather than a separately hand-rolled comparison, so the enabled check and the actual step can never disagree) | ✅ 2026-08-22 (`AdminVM.swift::nextPage`/`prevPage` call `nextPageOffset`/`prevPageOffset` directly; the hand-rolled guards had existed since 2026-06-08, missed by every prior sweep — compile-verified twice, full `swift-test` pass blocked by build-slot contention, see § Implementation status today) | ✅ (same shared FaunaKit `AdminVM`) | ✅ 2026-08-22 (`AdminUsersVM.kt::nextPage`/`prevPage` call `com.fauna.ffi.{nextPageOffset,prevPageOffset}` directly — the users-list pagination gap closed with the display pair above in one pass) | ✅ 2026-08-03 (direct call, `admin/mod.rs::next_users_offset`/`prev_users_offset`) |
| Per-alias rate cap (`parse_count_i64`/opt) | ✅ | ✅ | ✅ | ✅ 2026-06-29 | ✅ | ✅ (all, 2026-06-29) | ✅ 2026-08-17 (`settings/mail_aliases.rs::submit_action` calls `parse_count_i64` + `parse_count`. ⚠ This cell read "not yet built in tui" until then — **stale, and the staleness hid a live bug**: the add/edit sheet exists and its bare `.parse::<i64>().ok()` accepted a negative hourly limit onto the wire) |
| Serving-status label | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ (all six read sites; 2026-07-02 — `mail_serving_status_label` fn contract owned by `admin.md` § 2 Users) | ✅ (`admin/users.rs:353` calls `mail_serving_status_label` directly) |
| DNS verdict label | ✅ 2026-07-02 (+ `observed` 2026-07-29) | ✅ 2026-07-02 (+ `observed` 2026-07-29) | ✅ 2026-07-18 (`AdminDnsPage.VerdictDisplay` → `dnsVerdictLabel`; local switch deleted, brush/colour derivation kept local) (+ `observed` 2026-07-29) | ✅ 2026-07-17 (`AdminDnsView.verdictText` → `dnsVerdictLabel`; local switch deleted) (+ `observed` 2026-07-29) | ✅ 2026-07-17 (same shared FaunaKit view) | ✅ 2026-07-17 (`AdminDnsScreen.verifyStatusLabel` calls `dnsVerdictLabel`; the local `when(status)` deleted) (+ `observed` 2026-07-29) | ✅ 2026-08-17 (`admin/dns.rs::record_status_text` calls `dns_verdict_label`, mapping the typed enum to its serde variant name exactly as the same page's `cert_status_text` already did; byte-identical output, so the pin is the equivalence test `every_record_status_spelling_comes_from_the_shared_verdict_label` — the fn had no direct coverage at all before) |
| Cert status badge (`cert_status_view` — state word **+** which sub-label) | ✅ 2026-07-16 (`certStatusView`; the local `is_floor`/`not_after_unix` chain deleted) | ✅ 2026-07-16 (`cert_status_text` calls `cert_status_view`; the local chain deleted) | ✅ 2026-07-18 (`AdminDnsPage.CertStatusDisplay` → `certStatusView`; the local `is_floor`/`not_after_unix` chain deleted, incl. the latent `_ => status_expiring` catch-all that inverted the shared `status_on_floor` fallback — now fixed) | ✅ 2026-07-17 (`AdminDnsView` → `certStatusView`/`certStateWord`; the local `certStateText` state→key map + the `is_floor`/`not_after_unix` sub-label chain deleted) | ✅ 2026-07-17 (same shared FaunaKit view as macos) | ✅ 2026-07-17 (`AdminDnsScreen.certStatusText` calls `certStatusView`; the local `when(cert.state)` + `isFloor`/`notAfterUnix` chain deleted) | ✅ 2026-07-29 (`admin/dns.rs:132` calls `fauna_core::format::cert_status_view` directly) |
| Backup destination status labels (`backup_last_upload_label` + `backup_backlog_label`) | ✅ 2026-07-16 (`$lib/value-format::{backupLastUploadText,backupBacklogText}`; the hand-rolled `secs > 0` guard + `secs * 1000` deleted) | ✅ 2026-07-15 (`views/backups/destinations.rs` via `i18n::backup_last_upload`/`backup_backlog`; the hand-rolled guard deleted) | ✅ 2026-07-16 (`BackupsViewModel.{LastUploadText,BacklogText}`; the missing epoch-`0` guard drift closed) | ✅ 2026-07-16 (`BackupDestinationsVM.{lastUploadText,backlogText}`; one shared FaunaKit VM covers macos+ios; the missing epoch-`0` guard drift closed) | ✅ 2026-07-16 (same FaunaKit VM as macos) | ✅ 2026-07-19 (`BackupDestinationsSection.{lastUploadText,backlogText}`; the missing epoch-`0` guard drift closed) | — (backups page not yet built in tui) |
| Backup **audit** labels (`backup_last_audit_label` + `backup_audit_alert_label`) | ✅ 2026-07-30 (`$lib/value-format::{backupLastAuditText,backupAuditAlertText}` over the new wasm exports; `reason` crosses opaquely) | ✅ 2026-07-29 (`views/backups/destinations.rs::{last_audit_text,render_alerts}` via `i18n::{backup_last_audit,backup_audit_alert}`, direct core calls — first shell) | ✅ 2026-08-02 (`BackupsViewModel.{BackupLastAuditLabel,BackupAuditAlertLabel}` calls — windows was the 7th and last app to land the audit-alert surface) | ✅ 2026-08-01 (`BackupDestinationsVM.swift`, shared FaunaKit) | ✅ 2026-08-01 (same shared FaunaKit VM as macos) | ✅ 2026-07-31 (`BackupDestinationsSection.{lastAuditText}` + `BackupAuditAlertsContent`, over the new `com.fauna.ffi.{backupLastAuditLabel,backupAuditAlertLabel}` UniFFI exports — `reason` crosses opaquely) | ✅ 2026-07-29 (`backups.rs`, direct core calls — second shell) |
| Backup **client-device** labels (`backup_destination_kind_label` + `backup_destination_kind_options` + `backup_usage_label` + `parse_byte_size` + the `every_destination_is_a_client_device` predicate) | ✅ 2026-08-03 (`backups/+page.svelte` via `$lib/value-format::{backupDestinationKindText,backupUsageText,…}`; the client-custodian kind's android + web UI legs landed same-commit as android) | ✅ 2026-08-03 (`views/backups/destinations.rs`, direct core calls — second shell) | ✅ 2026-08-04 (`BackupsViewModel.{BackupDestinationKindLabel,BackupUsageLabel,BackupDestinationKindOptions,ParseByteSize}`) | ✅ 2026-08-03 (`BackupDestinationsVM.swift`, shared FaunaKit) | ✅ 2026-08-03 (same shared FaunaKit VM as macos) | ✅ 2026-08-03 (`BackupDestinationsSection.kt`, direct wasm-sibling UniFFI calls) | ✅ 2026-08-03 (`apps/fauna-tui/src/backups.rs`, direct core calls — lead shell) |
| Factor weight (`parse_weight_permille`) | ✅ (verified 2026-07-10 — `routes/feed/+page.svelte` calls `parseWeightPermille`; the lenient `parseFloat`/`Math.round` drifts closed) | ✅ 2026-07-11 (`views/feed/feed_list.rs` calls `fauna_core::format::parse_weight_permille` directly; the hand-rolled `parse::<f64>` + `* 1000.0` pair deleted) | ✅ 2026-07-09 (+ real-FFI `FactorWeightParseTests`) | ✅ 2026-07-18 (`FeedCreateForm.swift::addFactor()` calls `parseWeightPermille`; the hand-rolled `Double(newFactorWeight)` + `Int64((weight * 1000).rounded())` pair deleted). One shared `FeedCreateForm.swift` backs both macos/ios. | (same shared `FeedCreateForm.swift` as macos — see that cell) | ✅ (`FeedScreen.kt:1726` calls `com.fauna.ffi.parseWeightPermille` — consumed from the editor's first build; this cell read "no create-feed factor editor" until sweep caught it stale) | ✅ (`feed/mod.rs:420` calls `fauna_core::format::parse_weight_permille` directly) |
| Factor weight display (`format_weight_permille`) | ✅ 2026-07-18 (`routes/feed/+page.svelte::factorLabel` calls `formatWeightPermille`; the hand-rolled `toFixed(2).replace(...)` deleted) | — (`views/feed/feed_list.rs:636-637`'s factor summary lists factor names only, never renders the weight — no hand-roll to unify; a separate product-surface question, not this lift's scope) | ✅ 2026-07-18 (`FeedPage.xaml.cs:872` calls `FormatWeightPermille`; the hand-rolled `weightPermille/1000.0:"0.##"` deleted) | ✅ 2026-07-18 (`MacFeedFormView.swift:112` calls `formatWeightPermille`; the fixed 1-decimal `String(format: "%.1f", ...)` hand-roll deleted) | ✅ 2026-07-18 (`FeedFormView.swift:111`, same swap, separate call site) | ✅ (`FeedScreen.kt:1670` renders `×${com.fauna.ffi.formatWeightPermille(..)}` — consumed from the editor's first build; stale "no editor" cell caught by sweep) | — (tui's factor list shows factor names only, same gap as linux) |
| Absolute local timestamp display (`format_unix_local`) | n/a — uses its own locale-native `.toLocaleString()`/`fmtEpochSecs` instead, not this fixed format; pre-existing divergence, untouched | ✅ 2026-07-17 (`settings/mail.rs::build_credential_row` + `settings/linked_nests.rs::{grant_row,history_line}`; both local `format_unix_local` copies deleted) | ✅ 2026-07-18 (three independent local copies deleted — `NestTrustFormat.FormatLocalTimestamp` (nest-trust grant/history rows) + `MailSettingsPanel.FormatUnixLocal(ulong)` (mail-credential created-at); all call sites now route through `FaunaFfiMethods.FormatUnixLocal`. ⚠ RE-decayed and re-closed 2026-08-17, sweep: three NEW hand-rolls had appeared since — `BackupsViewModel.FormatEpochSeconds` + `MediaPage.FormatTimestamp` (seconds) and `AdminBridgesPendingPage.FormatFirstSeen` (ms, via the new `FormatUnixLocalMs` door); all three re-routed, compile pending on Windows) | n/a — locale-native (the `—` "no consumer surface yet" claim had decayed: datetime surfaces exist — `RestoreVM.formatWhen`, `AdminBridgesPendingView.formatFirstSeen`, `APIClient.epochDisplayString` — all locale-style renders, the same sanctioned class as web/android; verified 2026-08-17, sweep) | n/a — (same shared FaunaKit views as macos, same verification) | n/a — uses its own locale-native `DateFormat.MEDIUM/SHORT` instead, not this fixed format; pre-existing divergence, untouched | ✅ (`settings/atproto.rs`, `settings/mail.rs` call `format_unix_local` directly) |
| Absolute local **date** display (`format_unix_local_date` + its `_ms` adapter) | n/a — same locale-native divergence as the row above | ✅ 2026-08-17 (`i18n::local_date` is now a bare alias for `format_unix_local_date_ms`; its own `ms / 1000` — which truncated toward zero — deleted) | ✅ 2026-08-17 (sweep — the `—` "no consumer surface yet" cell had decayed: FIVE fixed-`"yyyy-MM-dd"` hand-rolls existed, `AdminDnsPage.FormatCertExpiry` (seconds) + four byte-identical private `FormatMillisLocal` copies in the mail-aliases/spam/lists/list-members VMs (ms). All five now route through the new `FormatUnixLocalDate`/`FormatUnixLocalDateMs` UniFFI exports; compile pending on Windows) | n/a — locale-native date surfaces exist (`AdminDnsView.formatDate` `.abbreviated`, `MediaExplorerContent`/`MediaItemDetailView`/`AtprotoSettingsView`); same sanctioned class as web/android, verified 2026-08-17 (sweep) | n/a — (same shared FaunaKit views as macos) | n/a — same locale-native divergence as the row above | ✅ 2026-08-17 (all **five** private `from_timestamp_millis(..).format(..)` copies deleted — `format::{format_epoch_us, epoch_secs_date, backup_last_upload, backup_last_audit}` + `conversations::local_short_date`; they rendered EMPTY out of range where the shared contract renders the raw number. Guarded tree-wide by `format.rs::no_painted_text_hand_rolls_a_shared_owned_local_date`) |
| Grace countdown (`grace_countdown` — mail primary-domain-rename `admin-dns-rename` banner) | ✅ 2026-07-19 (`admin/dns/+page.svelte::graceRemaining` calls `graceCountdown`; the hand-rolled `ms → days/hours → "{d}d {h}h"` chain with baked-in unit letters deleted) | ✅ 2026-07-19 (`views/admin.rs::grace_remaining` calls `crate::i18n::grace_countdown`; the identical local chain deleted) | ✅ 2026-08-25 (`AdminDnsPage.xaml.cs:1600` `GraceRemaining` calls the shared `ValueFormat.GraceCountdown` (`FaunaFfiMethods.GraceCountdown` + `Strings.Resolve`); the hand-rolled `ms → days/hours → "{d}d {h}h"` chain deleted) | ✅ 2026-07-20 (`AdminDnsView.swift::graceRemaining` calls `FaunaFFISwift.graceCountdown`; the hand-rolled `ms → days/hours → "{d}d {h}h"` chain deleted) | ✅ 2026-07-20 (same shared FaunaKit view as macos) | ✅ 2026-07-19 (`AdminDnsScreen.kt::graceRemaining` calls `com.fauna.ffi.graceCountdown` + `localized()`; the identical local chain deleted) | — (tui's `admin-dns-rename-banner` (`admin/dns.rs::rename_banner_elements`, landed 2026-07-29) shows state + domains only; no grace-countdown reveal built) |
| Bunker roster labels (`bunker_app_label`/`bunker_last_used_label`) | ✅ 2026-07-23 (`NostrSettingsSection.svelte` calls `bunkerAppLabel`/`bunkerLastUsedLabel`; the hand-rolled ternary + `Option`-branch deleted) | ✅ 2026-07-23 (`settings/nostr_tab.rs::{bunker_app_label,bunker_app_meta}` call the shared fns directly; the identical local chain deleted) | ✅ 2026-07-30 (`NostrViewModel.cs::BunkerAppRow.From` calls `FaunaFfiMethods.BunkerAppLabel`/`BunkerLastUsedLabel`, apple's shape chosen over android's hand-roll) | ✅ 2026-07-24 (`NostrSettingsView.swift` calls `bunkerAppLabel`/`bunkerLastUsedLabel`) | ✅ 2026-07-24 (same shared FaunaKit view as macos) | ✅ 2026-08-21 (`NostrScreen.kt::ConnectedAppsSection` calls `com.fauna.ffi.bunkerAppLabel`/`bunkerLastUsedLabel` + `localized()`; the hand-rolled `ifEmpty`/`?:` branch deleted — row 357) | ✅ 2026-07-23 (`nostr.rs::{bunker_app_label,bunker_app_meta}` call the shared fns directly; the identical local chain deleted) |
| Event count (`event_count` — month-grid `events-day-cell` accessibility tooltip) | — (MiniCalendar has no day-cell tooltip yet) | ✅ 2026-08-26 (`views/events/month_grid.rs` calls `crate::i18n::event_count` directly; the hand-rolled "1 event"/"{} events" `if`/`else if` branches — a hardcoded-English literal — deleted) | — (no day-cell tooltip yet) | — (no day-cell tooltip yet) | — (same shared FaunaKit month view as macos) | — (no day-cell tooltip yet) | n/a — no mouse-hover concept |

Dated one-line history (details in git):

- **2026-06-01 → 06-06:** the byte/duration/relative trio lifted + adopted fleet-wide (web → android → windows → apple; apple's `ValueFormat.swift` + `ValueFormatTests` cross-language conformance, 71 tests; two stray apple byte-size sites re-unified 2026-07-02).
- **2026-06-03/14/15:** provisioning elapsed + step display lifted and adopted (windows origin, tracked internally; apple `ProvisioningStepRow` deletes its local map).
- **2026-06-04 → 07-01/02:** short id (Phase 2, all six), hex_full (windows, then apple), serving-status + dns-verdict/cert-status + confidence-percent lifts (windows/apple serving-status via the shared `AdminUsersHubView`).
- **2026-06-27/28/29:** the validation family (port, tier cap, mail knobs, per-alias rate cap) lifted + adopted on all five app codebases + web; apple's cap-edit zeroing bug fixed by conforming to the shared fall-back-to-prev rule.
- **2026-07-06 → 07-10:** the net-new sweep lifted `short_nest_id` + `account_display_label` (windows/web consumed; linux's direct-call adoption verified in code 2026-07-10 — the earlier "still owed by linux" notes were stale); `parse_weight_permille` landed with windows + web consumed (web verified 2026-07-10).
- **2026-07-15:** backup-destination status labels lifted (`backup_last_upload_label` + `backup_backlog_label`, closing the apple/android missing epoch-`0` guard drift); linux consumed same-commit, the other five legs entrusted per app NEXT.
- **2026-07-16:** apple (macos + ios, one shared FaunaKit VM) consumed backup-destination status labels; also consumed `render_document_has_blocked_remote_images` (render-model.md § Implementation status — apple was the last of six apps on that face).
- **2026-07-16:** windows consumed backup-destination status labels (`BackupsViewModel.{LastUploadText,BacklogText}`; windows also lacked the `> 0` guard, so this closed the same epoch-`0` drift apple/android had).
- **2026-07-16:** the subscription author label lifted (`author_display_label`, § Subscription author label), closing the whitespace-only-handle blank-cell drift on linux/apple/android/windows; web + linux consumed same-commit, the three native legs entrusted per app NEXT. Supersedes the "subscriptions/payments formatting is sweep-confirmed DRY" note — that sweep covered price/status/counts, not the handle-else-hex chooser.
- **Sweep coverage note (2026-07-06):** multi-domain handles, box-recovery, recipient-whitelist, account-switcher, legal-takedown tombstones, muted-keywords, moderation-queue supersets, folder sharing, android P2P reshape — all re-checked, confirmed DRY or too immature to have duplicated; no other lift candidates found.
- **2026-07-18:** `format_weight_permille` (the `parse_weight_permille` inverse — display, not parse) lifted; web consumed same-commit. Standing-watch sweep also found the `parse_weight_permille` row's macos/ios "no create-feed factor editor" cell was **stale** — the editor exists (`FeedCreateForm.swift`) and hand-rolls both directions without calling either shared fn; corrected in place, both legs entrusted. Windows' existing `parse_weight_permille` consume is unaffected; only its display leg is new/open.
- **2026-07-18:** windows' mechanical consume batch #2 closed five of its remaining open legs in one session: `author_display` (Subscription author label), `dns_verdict_label` + `cert_status_view` (the latter also fixing the latent unknown-`CertHealthState` fallback that inverted `status_on_floor`↔`status_expiring`), `format_weight_permille` (Factor weight display), and `format_unix_local` (Absolute local timestamp display — windows had **three** independent local copies, not one; all three deleted). Windows is now the only app whose per-row adoption cells for these five formatters are entirely closed (no `⏳` remaining across them).
- **2026-07-18:** apple (macos + ios, one shared `SubscriptionSettingsView.swift`) consumed the subscription author label — read `sub.authorDisplay` directly at both call sites, deleted the local `authorLabel(_:)` helper (the same whitespace-only-handle blank-cell drift the other four apps had). Subscription author label's per-row adoption cells are now entirely closed (no `⏳` remaining across any of the six apps).
- **2026-07-18:** apple (one shared `FeedCreateForm.swift` + its two platform form views) consumed both factor-weight legs — `addFactor()` now calls `parseWeightPermille` (deleting the lenient `Double(newFactorWeight)` + local rounding), and `FeedFormView.swift`/`MacFeedFormView.swift` both call `formatWeightPermille` (deleting the fixed 1-decimal `String(format: "%.1f", ...)` hand-roll, which lost precision the parse leg itself preserves). Both factor-weight rows' per-app adoption cells are now closed everywhere a create-feed factor editor exists (android has none).
- **2026-07-21/22:** web built its owner-side "Shared with" roster (`accountDisplayLabel` called directly per row, no pre-computed field on its wire type) and its recipient-side pending-share/owner-badge UI (`FoldersSection.svelte` reads `share.sharedByDisplay`/`fs.owner_display` verbatim) — the pending-share-sharer-label and owner-side-folder-badge rows are now closed on web, the last of six original apps (windows had in fact already consumed both, 2026-07-14/15).
- **2026-08-02:** `device_display_identity` lifted (§ Device display identity) with the guardian ward-device display-identity ruling (`family-safety.md` § Full visibility for young children). Deliberately **absent from the adoption matrix**: its one consumer is the *nest's* `fauna.family.status` projection, so all 7 apps render the substituted wire field verbatim with zero app-side code — there is no per-app leg to track.
- **2026-08-05:** that fn **replaced** by the set-level `device_display_identities` (§ Device display identity): its per-device signature could not keep the *per-device distinct* guarantee `family-safety.md` § Full visibility states, because `device_id` is client-chosen and two devices can collide both on a rendered id prefix (adversarially) and on a shared machine label (`SELF_REGISTER_LABEL`, with no adversary). The row now always carries the device code, widened set-relatively, with any machine label beside it. Still absent from the adoption matrix for the same reason — one nest-side consumer, zero app-side code.
- **2026-07-23 sweep:** tui column added to the adoption matrix (tui reached its 7-app parity milestone 2026-07-19, but this doc's table was never extended) — tui is Rust-native like linux and directly calls most of these fns, but hand-rolls port validation and tier-cap validation (`admin/mod.rs`) instead of `parse_port`/`parse_cap`, both flagged `⏳`. Also corrected: the pending-share/owner-badge rows' web+windows `⏳` cells and the "Account display label" section's second/third/fourth-consumer prose had drifted stale since 2026-07-14/15 (windows' consumes long predated this doc's own claims to the contrary); the dangling "§ Serving-status" self-reference (no such section exists in this file) fixed to point at `admin.md` § 2 Users, the fn's actual owner.
- **2026-08-03:** `next_page_offset`/`prev_page_offset` lifted (§ Pagination) — the stepper half of the `total_pages`/`current_page` display pair, previously (and now-incorrectly) described in this doc as "too trivial on its own to earn a shared fn." tui and linux consumed same-commit; web consumed the same day over a new wasm export. New row added to the adoption matrix (sweep, below).
- **Sweep (2026-08-14):** re-verified tui's tier-cap-validation and windows' pagination-display open legs — both still accurate at the cited file:lines. Found and fixed four stale adoption-matrix cells the 2026-07-24 sweep had missed or that closed since: windows' and android's "Account display label" cells (both had in fact consumed `account_display_label` from their multi-account switchers' first builds, 2026-07-18/19 — predating even the 2026-07-24 sweep); apple's "Short nest id" cells (box-recovery Task E, landed 2026-07-31, calls `shortNestId` directly); and the "Backup audit labels"/"Backup client-device labels" rows, both now fully ✅ across all 7 apps (windows landed the last legs of each, 2026-08-02 and 2026-08-04). Also added the wholly-undocumented `next_page_offset`/`prev_page_offset` pagination-stepper lift (landed 2026-08-03, joins the existing `total_pages`/`current_page` § Pagination section) as a new matrix row — its own Rust doc comments already pointed back at "§ Pagination" for a write-up that didn't exist yet.

- **Sweep (2026-08-17, a standing shared-Rust harvest):** documented the wholly-undocumented **date-only** sibling of § Absolute local timestamp display (`format_unix_local_date`, in shared Rust since the datetime lift) and added its epoch-**milliseconds** door `format_unix_local_date_ms`, then closed the five-copy hand-roll it had been missing. This section is why the copies existed: it named a home for the `HH:MM` form and declared "no native app hand-rolls this format anymore", while the date-only form it also owns had no written home and — crucially — **no ms entry point**, so every ms-valued caller wrote its own chain. tui had five (`format::{format_epoch_us, epoch_secs_date, backup_last_upload, backup_last_audit}`, `conversations::local_short_date`); all five ended `.unwrap_or_default()`, rendering **empty** out of chrono's range where this section's contract renders the raw number — i.e. a corrupt timestamp read exactly like an unset one. linux never had the bug (one `i18n::local_date` delegate), which is the same asymmetry the 2026-08-17 calendar-name harvest found in the other direction. In-range output is byte-identical before and after; the only behaviour change is that out-of-range fallback. The ms→secs floor (`div_euclid`, not `/`) also moved into shared Rust, fixing linux's own pre-1970 sub-second edge in passing. Guarded on tui by a tree-walking sibling of the month/weekday-name guard.

- **Sweep (2026-08-17, same harvest, continued):** closed **both** remaining tui open legs, each verified still-open against current code first. `parse_cap` (§ Tier cap validation) was carrying a **live bug**, not just duplication: `tier_update_req`'s bare `.trim().parse::<i64>()` succeeds on `-5`, so a negative allowance rode the wire on a row that looked valid — the negative clamp is the one semantic a bare parse cannot reproduce, and it had no test. Its `None` consume shape turned out to be the *richer* one and was deliberately kept, with the two sanctioned shapes now ruled in that section. `dns_verdict_label` was pure duplication with byte-identical output — pinned by an equivalence test rather than a behavioural one, since no behavioural test could distinguish them and `record_status_text` had no direct coverage at all (which is how it stayed the last un-adopted leg on a page whose sibling `cert_status_text` had already consumed its shared view — the same *sibling* blind spot sweep found one level up in this doc's own prose). Then a **third** leg the open-legs list did not know about, found by grepping tui for `parse::<i64>` rather than trusting the matrix: the per-alias rate cap's tui cell said "not yet built in tui" — **stale, and the staleness was hiding the same live bug**. The add/edit sheet exists (`settings/mail_aliases.rs::submit_action`) and its bare `.parse::<i64>().ok()` accepted a negative hourly rate limit onto the wire, which is precisely the "natives' incidental `long`-parse drift" `parse_count_i64`'s `>= 0` filter was written to eliminate; its `spam_threshold_override` sibling was byte-identical duplication of `parse_count`. Both now call the shared fns. ⚠ **The lesson across all three legs: a `—` cell ("no consumer surface yet") is a weaker claim than a `⏳` cell and decays silently** — nothing re-checks it when the surface lands, so it reads as "nothing to do here" exactly when a new hand-roll has appeared. Two of this sweep's three negative-clamp bugs were sitting behind such a cell or behind a per-function claim that never named the sibling. **tui now has no open consume leg in this doc**; the complete remainder is windows-side.
- **Sweep, tail (2026-08-17):** having stopped trusting the matrix, the sweep grepped tui for *every* remaining raw numeric parse and classified all ten. Seven are correct as written and deliberately left — three delegate range validation to the nest with a documented default (`dns_rename_grace_days`, `dns_rename_extend_days`, `invite_uses_input`'s `.max(1)`), one raises an explicit typed error that an `Option`-returning shared fn would *lose* (`max_free_users`), one parses a terminal escape response rather than user input (`graphics/detect.rs`), and the rest carry no shared counterpart. **Two were real.** `settings/mail_lists.rs`'s per-send cap was byte-identical duplication of `parse_count` (no bug — a `u32` parse rejects a negative by itself). `settings/mod.rs`'s WireGuard `listen_port` was a **third port field** the § Port validation section had never named, hand-rolled as a bare `.parse::<i64>()`, so `0` / `-1` / `65536` all rode the wire as a peer's listen port — `0` being exactly the value the canonical rule calls out as unbindable. ⚠ The reason it shipped unobserved generalizes: it sat **inline in a handler arm that needs a live nest**, so no unit test could reach it. It is now the pure `P2pState::listen_port`, the same extraction `admin::tier_update_req` exists for. **A validator inlined into an un-unit-testable arm is an unobserved validator** — extract it, and the test follows for free.

- **Sweep (2026-08-17, same harvest, next app):** ran sweep's raw-operation grep over **linux** (all 15 raw numeric parses classified) and, transferring the instrument, over **web** (`parseInt`/`parseFloat`, 11 hits). Thirteen linux sites are correct as written, each under a class this doc or sweep already ruled: the rename grace/extend and invite-uses fields are the same delegate-range-to-nest / `.max(1)` shapes tui's were ruled correct under (linux's extend even guards `n >= 1` locally), `max_free_users` renders its typed hint on failure, and the rest parse internal round-trips (own dropdown keys, own widget markers, `%z` offsets, DB strings), env-set IPC ports, or e2e-agent surfaces. **Two were lifts:** the tier-rank hand-roll (see below) and `mail_lists.rs::parse_opt_u32` — byte-identical duplication of `parse_count`, the exact twin of tui's sweep-164 find, now deleted. **Web carried the live bug:** the profile Tiers form's `parseInt(formRank.trim(), 10) || 0` — lenient (`"5abc"` → 5) *and* negative-passing (a negative is truthy), with the wasm `u32` ABI turning `-3` into `4294967293`, a tier silently outranking every other. Six-app comparison showed five natives already agreeing on the canonical rule, so the fix was a new one-paragraph **§ Tier rank** naming `parse_count` reuse (no new fn), consumed on web/tui/linux same-commit; windows/apple/android carry behaviorally identical hand-rolls (matrix `⏳`, consume-lift only, no bug). Web's remaining raw parses are internal machine-format reads (hex nibbles, label `category:confidence` strings, own select values) — correct, with one noted non-numeric cleanup candidate: three local copies of `hexToBytes` beside `$lib/hex.ts`'s own (`rpc.ts:3138`, `wasm-atproto-settings.ts:52`, `feed/+page.svelte:1043`), a web-internal dedup, not a validation gap.

- **Sweep, tail (2026-08-17):** completed the raw-operation classification across the remaining three app codebases from source (fixes verified only where this machine builds): **windows'** three signed `TryParse` sites and **android's** eleven `OrNull` string parses all fall under already-ruled classes (delegate-range-to-nest grace, guarded extend/uses — android's invite clamp lives one layer down in `AdminUsersVM.createInviteCode`'s `coerceAtLeast(1)`, correct — internal round-trips, consumed shared fns). **Apple and web were the two invite-uses outliers**: apple's `AdminUsersHubView.confirmCreateInvite` parsed `Int(maxUses) ?? 1` with no clamp, and web's `min="1"` number attribute is decorative (typing a negative still binds) — while the nest stores `uses` unvalidated and redemption checks `uses_left > 0`, so a `0`/negative mint is a **born-dead code** the admin hands out believing it works. Both now clamp `>= 1` like the other four apps (2026-08-17); invite-uses stays a ruled-correct local-clamp class per sweep, not a new shared fn. Bonus matrix correction: **both android factor-weight cells** read "no create-feed factor editor" while `FeedScreen.kt`'s editor exists and had consumed `parseWeightPermille`/`formatWeightPermille` from its first build — the `—`-cell decay class again, this time hiding done work rather than a bug.

- **Sweep (2026-08-17, same harvest, the `—` cells):** ran the vein sweep's close named — verify the three `—` cells in the date-only matrix row, the cell class sweep proved decays silently. **All three had decayed, windows twice over.** Windows: five fixed-`"yyyy-MM-dd"` hand-rolls behind the date-only `—` cell (`AdminDnsPage.FormatCertExpiry` seconds; four **byte-identical** private `FormatMillisLocal` copies in the mail-aliases/spam/lists/list-members VMs, ms) — plus, one row up, the ✅ full-form cell had RE-decayed: three new `"yyyy-MM-dd HH:mm"` hand-rolls postdating the 2026-07-18 "all call sites route through the shared fn" claim (`BackupsViewModel.FormatEpochSeconds`, `MediaPage.FormatTimestamp`, and the ms-valued `AdminBridgesPendingPage.FormatFirstSeen`, whose ms input is what forced the new door). Fixed by adding `format_unix_local_ms` to `fauna_core` (the full-form twin of sweep's date-only ms door, same `div_euclid` floor, pinned by a deterministic minute-straddle test rather than the date door's midnight search) and the missing UniFFI exports (`format_unix_local_ms`, `format_unix_local_date`, `format_unix_local_date_ms`); all eight windows sites now route through `FaunaFfiMethods` (compile-unverified — landed from the primary dev VM, which has no MSBuild; the C# binding regenerates at build on Windows). Apple's two `—` cells decayed differently: date/datetime surfaces exist (`RestoreVM.formatWhen`, `AdminDnsView.formatDate`, `MediaExplorerContent`, `MediaItemDetailView`, `AtprotoSettingsView`, `APIClient.epochDisplayString`) but ALL render locale-natively — the sanctioned web/android divergence class, so those cells become `n/a` with citations, no code change. Also classified: apple's ms→secs conversions are `Double` division (no truncation class — the `div_euclid` hazard is integer-only), and the two watchOS `/ 1_000_000` sites are CORRECT (`DecodedEmail.timestamp` is `fauna_core::data::Timestamp` — **microseconds**, `data.rs:58` — not the ms most FFI timestamps carry; do not "fix" them to `/ 1_000`). Vein note: no new open legs — the windows consume landed with the sweep; the only residue is the win compile-verify. Same sweep also closed sweep's other noted candidate: web's three local `hexToBytes` copies are deleted, the non-validating fast path now lives beside `bytesFromHex` in `$lib/hex.ts` (no import cycle — `rpc.ts` already runtime-imported `$lib/wasm`, `hex.ts`'s only runtime dep).
- **Sweep (2026-08-17, nest side) — the sweep turned around: the *nest* side of the same class.** *(Numbered 167 on resolve: sweep above landed the same day from a parallel session on a different vein — the `—` cells — and its finding that the two watchOS `/ 1_000_000` sites are correct was reached independently here too.)* Sweeps 164–165 asked "does any app send an out-of-range value?" and closed the class on all 7 apps. The mirror question was never asked, and it is the one that actually binds: **does the door refuse it?** An app clamp cannot answer for the nest, because a client may be **older** than the nest ([`../architecture/version-compatibility.md`](../architecture/version-compatibility.md)) and every clamp above landed 2026-08-17 — so a pre-clamp build still puts the raw value on the wire. Two doors had no check at all, and both failed *silently* rather than loudly: `fauna.admin.tiers.{create,update}` accepted a negative cap (the storage gate compares `used + delta > max_storage_bytes`, so one negative refuses **every** write by that tier's users while the admin sees a successful save), and `fauna.admin.invite_codes.create` accepted `uses <= 0` — the born-dead code sweep's tail recorded here as a known nest-side gap while fixing the last two app legs. A **third** door was worse, because it deletes user data: `fauna.admin.gc` accepted a negative `grace_period_secs`, which *inverts* the window rather than shrinking it — GC keeps a blob iff `created_at > now - grace`, so a negative cutoff sits in the future, nothing can exceed it, and the fresh-blob grace is disabled outright (the in-flight-writer race it exists to prevent). All three now refuse `fauna.admin.invalid_params`; contracts in `api-layers.md` § Track C / C2, mutation grade **4/4 exact** (weaken either floor, over-correct both into a `>= 1` floor, disable the GC guard — each killed by its own boundary test; the GC one is killed only because the test asserts the details *string*, since that door answers `invalid_params` for backup-not-configured too). The other three numeric admin inputs are **correct as written** and should not be "fixed": `users.list` clamps `1..=500` with `offset.max(0)`, `audit.list` clamps `1..=1000`, and `set_registration_mode`'s `max_free_users` is `Option<u64>` — the best shape of all, where a negative is *unrepresentable* rather than merely rejected. Note the split: a **clamp** is right where an out-of-range value is harmless (pagination), a **refusal** where it is destructive (caps, grace windows, credential uses). ⚠ **The transferable rule, and it audits this doc's own rulings: an app-side parse may claim "delegates range validation to the nest" only where the door actually checks.** Sweeps 164–165 ruled several parses correct on exactly that ground; spot-checked here, the DNS-rename grace/extend fields **do** have their delegate (`GRACE_DAYS_MIN..=GRACE_DAYS_MAX`, typed `fauna.bridges.invalid_grace_days`, with rejection tests) — which is what makes it a sound class and made the two doors that lacked one findable. Before writing that phrase again, grep the handler.
- **Sweep (2026-08-18, nest side) — sweep's instrument on the half it did not run: the USER-plane doors.** Sweep classified every `fauna.admin.*` numeric input and said in terms *do not re-run the admin doors*. The mirror surface — the doors an ordinary account, a peer nest, or a bridge writes to — had never been asked. Instrument: every numeric field on a non-admin `*Request` in `libs/fauna-protocol` (a 1133-field scan narrowed to the ~110 signed ones, since a negative on a semantically-unsigned quantity is the exact defect shape), each traced to its handler and its consumer. **Two were real, and both are the *silent value change* kind rather than the missing-refusal kind.** **(1) `size_bytes` on the sync record plane** — the client-declared size **is** the storage meter (`admin.md` § 2 Users: the tier *is* the quota), and under retained accounting (slice 3, landed the same day) the charge simply **is** the declared size — so a negative one rides past the `charge > 0` ceiling check and then *credits* `storage_bytes_used`. One record with a large negative size floors a full account to 0 and the tier is spendable again — unbounded quota evasion for the price of one RPC, on a plane where the chunk-upload routes meter nothing themselves. Measured red first: the record returned `Ok(seq)`. Refused now in the single metering core every record door funnels through (`fauna.sync.changes.record`, both `fauna.federation.*.changes.record` relays, `fauna.bridges.webdav_record_change`; a fifth, the `/sync/ws` `FileChanged`, left with that data plane on 2026-10-02), so a new door cannot be added past it; contract in [`file-sync.md`](file-sync.md) § Multi-writer shared sets. Note the sibling door `fauna.index.record` had refused a non-positive `size_bytes` since it was written — one door of a class getting it right while four don't is what a chokepoint fixes and a per-door sweep does not. **(2) `listen_port`** — § Port validation above, the nest half of the field sweep fixed app-side; a bare `as u16` *wrapped* instead of refusing. Mutation-verified: reinstating the cast reddens both refusal pins and leaves the pass-through pin green. ⚠ **Classified CORRECT, do not "fix" these** (each was examined and each is right for a stated reason, several of them better shapes than a refusal): `byte_cap` on `folders.members.set_access` and `recipients_per_send` on the two account-list doors already refuse (the latter against an admin ceiling too); `limit` on `channel.fetch`, `folders.public.fetch` and the paginated lists routes through `effective_fetch_limit`, where `<= 0` is the documented "full page" the shipped clients send; `rescan_interval_secs` was a client-side cadence hint the nest was only a store for; its shared consumer `rescan_interval_from_secs` went with phase 5's de-knob (2026-08-20) and the field itself left the wire and the table 2026-10-01 (`sync-engine-deployments.md` § Cadence), so there is nothing left to classify; `ModerationModelSyncRequest`'s `ham_count`/`spam_count` are deliberately advisory (the real counters are derived from the decoded model, so an inflated claim buys nothing); `mta_sts_max_age_seconds` clamps at its DNS render. **And one that looks like sweep's `gc` window but is NOT:** `set_dkim_rotation_days` accepts `<= 0`, which does make the domain permanently rotation-due — but the mint job's own gate (`newest.selector != active || newest.selector == selector`) bounds it to one rotation per `<YYYYMM>` selector, so a `0` degrades to exactly what `1` already does (monthly rotation), never to key churn under a published selector. It was worth chasing precisely because it *reads* like the inverted-window class; the gate is the reason it isn't, and that gate is the thing to re-read before re-raising it. ⚠ **The rule this sweep leaves for the next one — sort the quantity by who produced it, not by whether it looks dangerous.** The nest's *other* quota, `inbox_bytes_used`, has no door check and needs none: `push_inbox_with_quota` charges `payload.len()`, so the number is **measured from bytes the nest is holding at that moment**. `storage_bytes_used` cannot do that — the chunks land out-of-band through the byte-plane routes, so the record only *declares* them — and the moment a meter reads a declaration, the door is the only thing between it and the client. The same split already runs through this codebase and each side got it right on its own: the backup-custody branch of this very function derives its charge from held bytes and the spam-model sync derives its counters from the decoded model, while `fauna.index.record` — which must trust a declaration — checks it. So: **a measured quantity is safe by construction; a declared one is a door obligation.** Grep for the declarations, not for the scary-sounding fields.

- **Sweep (2026-08-18, nest side) — sweep's other half: the *unsigned* fields, where the question is not the negative but the `0`.** Sweep classified the ~110 **signed** numeric fields on the user-plane doors and deliberately left the unsigned ones, because they fail differently. The mail/spam/auth/submission/IMAP/alias policy knobs (25 unsigned fields across the six `Put*PolicyRequest` types) ride as `Option<u32>`/`Option<u64>` — the best shape sweep named, where a negative is *unrepresentable* — so there is no negative question here at all. What is expressible is `0`, and `parse_count` accepts it: the apps save `parse_count(text).unwrap_or(prev)`, so an unparseable edit keeps the persisted value and **a `0` at a door is always a deliberate keystroke**, never a parse artifact. Each of the 25 was traced to its consumer. **⚠ The sweep's premise, "nothing in the tree records which is which", was false, and the correction is the more useful finding: TEN of the 25 already carried a recorded verdict**, spread across four places nobody had collected — struct doc comments (`greylist_delay_secs`, `unlisted_recipient_penalty`, `exact_aliases_max`), a handler comment that also *enforces* it (`put_spam_policy_handler`'s "`0` = disabled for a tier", with the ordering check skipping zero tiers), Go constructor comments (`PerIPLimiter` "a max of 0 means DISABLED", `authlock.New`'s zero sentinel), and a shared-Rust fallback (`effective_max_raw_message_bytes`: `0` is **not** uncapped, it falls back to the shipped default). Two more are floored in the consumer (`tombstone_retention_days` `.max(7)`; `idle_timeout_secs` → the 29-minute RFC 2177 default) and one is pinned by its own test (`training_history_retention_days = 0` prunes everything, deliberately). **The defect was the one knob whose neighbours all disagreed with it.** `max_conn_per_min = 0` made `RateLimiter.Allow`'s `count >= limit` trip on the **first** connection from every IP (count starts at 0), and `server.go` applies that at `NewSession` — so one typed `0` turned into `421` before the banner for every sender, forever: a total, silent inbound-mail outage. Its sibling in the same admin pane, `max_conn_per_ip`, means the **opposite** by explicit written decision (`internal/connlimit/perip.go`: "a max of 0 means DISABLED (admit everything) — the catalog row's `0 = disabled` sentinel — whereas the Rust crate's env-driven cap of 0 would reject everything"). Two per-IP connection knobs, adjacent in one pane, opposite `0` semantics, one documented and one not. Now disabled-at-zero like its sibling, red-verified. A second, narrower disagreement: `idle_timeout_secs` reaches **two** consumers off one field, and only one floored it — `Session.idle` fell back to the default while `Session.IdleTimeout()` handed the raw `0` to the vendored imapserver's read deadline; the accessor now applies the same rule, so the seam timer and the deadline cannot mean different things. ⚠ **Classified CORRECT, do not "fix" these:** every *allowance* knob's `0` (`max_per_day`, `storage_bytes_default`, `message_count_default`, `exact_aliases_max`). Their `0` does refuse everything — no submissions, no deliveries, no aliases — but that is the **ratified** `0`-is-meaningful boundary, not a bug: [`../architecture/api-layers.md`](../architecture/api-layers.md) § Admin doors refuses a *negative* tier cap and deliberately leaves `0` valid, "so the floor is `0`, never `1`", and the GC grace window keeps the same boundary. Refusing `0` here would contradict a ratified sibling ruling and delete an admin's legitimate "this account gets nothing". ⚠ **The transferable rule: sort the knob by what it *is*, not by how bad its `0` looks.** A **protection threshold** (a rate cap, a lockout, a score tier, a penalty) takes `0` = *the protection is off*; an **allowance** (bytes, messages, sends, aliases) takes `0` = *no allowance*. Both sentinels are correct, they are opposites, and every knob in a subsystem must be sorted before any of them is "hardened" — the one bug this sweep found was a protection knob implementing the allowance meaning, and it was invisible precisely because "0 rejects everything" looks defensible until you notice the knob beside it. **The sweep also found what is arguably the bigger defect, and it is not a `0` question at all:** five projected knobs reach **no consumer** — `retry_schedule_seconds`, `permanent_failure_timeout_hours`, `delay_warning_at_hours` and `ndr_rate_limit_days` (the only production call site of `retry_policy_from_outbound` passes `default_outbound_policy()`, not the stored override — its own doc comment admits it) and `max_recipients_per_message`. The admin sets them, the save succeeds, nothing happens — the same silent-success shape sweep called out for the tier caps. ⚠ **`max_recipients_per_message` is the one to study, because a check that looks exactly like its consumer exists and is not one:** `submission.go::Rcpt` rejects past `submissionToken.MaxRecipients`, but that token is minted client-side from a hard-coded `DEFAULT_MAX_RECIPIENTS` constant, so the admin knob feeds nothing — and the catalog row asserted it was "sealed as `MaxRecipients`" until this sweep measured it. That is sweep's false-delegation rule one level up: **a knob may only claim a consumer where the consumer actually reads *that knob*, not merely a field of the same name.** Grep the mint, not the check. Recorded in [`mail-policy-config.md`](mail-policy-config.md) § Implementation status today; wiring them is a separate track. Note that this is *why* `permanent_failure_timeout_hours = 0` was only a landmine and not an outage while dark: the retry engine gives up once `elapsed >= permanent_failure_after`, so a naive wiring would have made a stored `0` permanently fail every outbound message. **A dark knob does not mean a safe knob — it means an unexploded one.** **Wired 2026-08-19, landmine defused in the same commit:** `OutboundPolicyOverrides::effective()` (`db/mail_policy.rs`) resolves a stored `permanent_failure_timeout_hours = 0` to the catalog default instead of letting it ride through live; all five knobs now reach their consumers — `mail-policy-config.md` § Implementation status today.

- **Sweep (2026-08-22, standing shared-Rust harvest, apple leg) — a `—` cell decayed the other direction: not into a new hand-roll, but into a hand-roll that was already there before the cell was ever written.** § Pagination's stepper matrix row said apple had "no users-list pagination UI" for both cells. False for the stepper cell since **2026-06-08**: `AdminVM.swift::nextPage`/`prevPage` have hand-rolled the exact `next_page_offset`/`prev_page_offset` guards (`offset + pageSize < totalUsers` / `max(0, offset - pageSize)`) since before the shared fns existed (added 2026-08-03, sweep) — every sweep since then (162, 163–169) swept other apps' `—` cells for decay but never re-asked this one. True for the display cell: apple's admin-users hub still has no `"{current} / {total}"` render, only a total-user-count label + prev/next buttons, so `total_pages`/`current_page` genuinely have nothing to lift. Landed: `AdminVM.swift` now calls `nextPageOffset`/`prevPageOffset` directly (shared FaunaKit, ios inherits free). Byte-for-byte behavior-preserving — confirmed against `fauna_core::format`'s exact guard expressions before editing, not just "should be equivalent." **Verification scope, stated precisely rather than overclaimed:** `AdminVM.swift` itself compile-verified clean in two independent `swift build --package-path apps/fauna-apple --target FaunaKit` passes (zero errors attributed to this file, confirmed by grepping both full build logs for `AdminVM`). Two further attempts at the full `swift-test` recipe (needed for the `test-helpers` FFI flavor + the complete FaunaKit test suite) each queued 90 minutes on macOS's two build slots and timed out — both slots showed "DEAD (stale note; lock auto-released, retry should win)" holders the entire wait despite 78.5G free (well above the 40G floor), never actually reclaiming. Reads as a distinct build-slot bug (a stale lock note not translating into an actual reclaim under sustained fleet contention), not disk pressure — captured for follow-up, not chased further here. Matrix corrected: stepper row's macos/ios cells ✅, display row's macos/ios cells reworded from "no UI yet" to "has the UI, not the display" (they're a different claim, not just a date). ⚠ **The lesson this sweep adds to sweep's:** a `—` cell doesn't just decay when a *new* consumer surface appears after the cell was written — it can be **wrong from the cell's very first sweep**, if that sweep never actually greped the app's existing code before writing "no UI yet." Trust the grep, not the previous cell.

- **Sweep (2026-08-22) — android's § Pagination cells, the last un-`✅` app on the users-list pagination gap.** This sweep closed both rows in one pass: `AdminUsersVM.kt::nextPage`/`prevPage` call `com.fauna.ffi.{nextPageOffset,prevPageOffset}` directly (stepper row), and `AdminUsersScreen.kt`'s `UsersSection` renders the `"Page {current} of {pages}"` indicator via injected `com.fauna.ffi.{totalPages,currentPage}` (display row — android is the first app to render this indicator for the users list specifically, per sweep's finding that apple's own admin-users hub still has no such display). Both rows now read fully ✅ across all 7 apps — windows closed both 2026-08-25 (`AdminUsersViewModel.cs`, see the two matrix rows above).

- **2026-08-26:** `event_count` lifted (§ Event count), closing a hardcoded-English literal on linux's month-grid day-cell accessibility tooltip ("1 event"/"{} events" hand-rolled `if`/`else if` branches, `views/events/month_grid.rs`). Found during a standing shared-Rust recon pass — no other app has a day-cell tooltip yet, so the new matrix row is `—` everywhere but linux (tui `n/a`, no mouse-hover concept); consume-from-start when a GUI app gains one.

**Open legs (complete list, re-verified 2026-08-25; tui's two closed 2026-08-17; tier-rank legs added by sweep, android's closed 2026-08-21; windows' grace-countdown, quota-fraction and pagination legs all closed 2026-08-25):** the tier-rank consume on windows (`ProfileViewModel.cs:307`) and apple (`SubscriptionsVM.swift:213`, one shared FaunaKit VM) — both behaviorally identical to the shared rule, so these are drift-prevention lifts via the FFI `parse_count`, not bug fixes; batch them with each app's next trickle-down, not per-feature. Windows resw stays flat — named args substituted in C# (`S.Get`), per the i18n placeholder rule. The multi-account `account_display_label` legs (windows/android) and apple's `short_nest_id` recovery-UI leg (all three previously tracked here as open) are now fully closed — windows' and android's switchers had in fact consumed it from their own first builds (2026-07-18/19, missed by the 2026-07-24 sweep), and apple's box-recovery Task E (2026-07-31) calls `shortNestId` directly; corrected this sweep. The backup-label rows (audit + client-device) are likewise now fully closed on all 7 apps (windows landed both 2026-08-02/04, closing what the 2026-07-24 sweep had recorded as "FFI blocker lifted, only the render remains"); corrected this sweep.
