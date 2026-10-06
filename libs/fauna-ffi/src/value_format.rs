//! UniFFI façade for shared value formatting (`fauna_core::format`) — relative
//! time, byte sizes, and durations. The bucket / unit *decision* is computed in shared Rust
//! and returned as i18n keys (via `LocalizedText`), so the four native apps
//! localize through their own pipelines instead of hand-rolling the thresholds
//! or English strings. See `docs/goal/behavior/value-formatting.md`.
//!
//! `LocalizedText` (a `fauna_core` `uniffi::Record`) and `RelativeTimeDisplay`
//! cross the boundary directly — no FFI mirror, since fauna-ffi already enables
//! `fauna-core/uniffi` (transitively via the onboarding / folders machines).
//!
//! ## Gated element ids live in gated DOC LINES
//!
//! Several formatters here are ungated inert surfaces that a *gated* plane's
//! renders call — the tip formatters and `claim_status_label` are `payments`
//! (`dynamic-features.md` § Charter members). Naming their element ids in
//! ungated prose ships those ids in a store-safe artifact: `#[uniffi::export]`
//! embeds docstrings in the cdylib metadata and propagates them into the
//! generated Swift/Kotlin face, so an excised build would carry — and
//! *document* — the UI it excised. Criterion 1 is "prose included" exactly as
//! criterion 2 is (measured at **9**
//! `post-tip-` + **1** `subscription-claim-` residual lines).
//!
//! So the id-naming sentence rides `#[cfg_attr(feature = "payments", doc = …)]`
//! while the rest of the doc stays ungated. **Gate the line; never reword it** —
//! the kebab id stays spelled verbatim in this file, so `rg post-tip-total`
//! still finds every driver. `just ffi-store-safe-check` pins both halves: the
//! ids absent from the store-safe flavor, present in the default one.

use fauna_core::format::{
    BackupLastUploadDisplay, CertStatusView, ConversationTimestampDisplay, OfferStatus,
    RelativeTimeDisplay, SyncDisplayState,
};
use fauna_core::localized::LocalizedText;

/// `fauna_core::format::byte_size` → a `LocalizedText` `{ key, args }` the client
/// resolves through its i18n pipeline (e.g. `"size.mb"` + `{value: "1.5"}`).
#[uniffi::export]
pub fn byte_size(bytes: u64) -> LocalizedText {
    fauna_core::format::byte_size(bytes)
}

/// `fauna_core::format::relative_time_display` → `{ localized, absolute_epoch_ms }`:
/// render `localized` for recent items, else format `absolute_epoch_ms` with the
/// platform's native locale-aware date formatter (`≥ 7 d` old).
#[uniffi::export]
pub fn relative_time_display(now_ms: i64, then_ms: i64) -> RelativeTimeDisplay {
    fauna_core::format::relative_time_display(now_ms, then_ms)
}

/// `fauna_core::format::duration_secs` → a `LocalizedText` `{ key, args }` (e.g.
/// `"time.uptime_dhm"` + `{days, hours, mins}`) for coarse uptime/duration display.
#[uniffi::export]
pub fn duration_secs(secs: u64) -> LocalizedText {
    fauna_core::format::duration_secs(secs)
}

/// `fauna_core::format::tip_amount` → a `LocalizedText` `{ key, args }` the
/// client resolves through its i18n pipeline. See
/// `docs/goal/behavior/monetization.md` § Tips.
// `payments`-gated doc line — see § Gated element ids live in gated DOC LINES.
#[cfg_attr(
    feature = "payments",
    doc = " Rendered into `post-tip-total`, and into each `post-tip-item`'s amount."
)]
#[uniffi::export]
pub fn tip_amount(msats: i64) -> LocalizedText {
    fauna_core::format::tip_amount(msats)
}

/// `fauna_core::format::tip_count` → a `LocalizedText` `{ key, args }`, shared
/// so no client ships "1 tips". See `docs/goal/behavior/monetization.md` § Tips.
// `payments`-gated doc line — see § Gated element ids live in gated DOC LINES.
#[cfg_attr(feature = "payments", doc = " Rendered into `post-tip-count`.")]
#[uniffi::export]
pub fn tip_count(count: i64) -> LocalizedText {
    fauna_core::format::tip_count(count)
}

/// `fauna_core::format::tip_more` → a `LocalizedText` `{ key, args }` for the
/// "and N more" tail of a bounded attribution window. `n` is the caller's
/// `tip_count - senders.len()`, from the nest's own `has_more` + totals. See
/// `docs/goal/behavior/monetization.md` § Tips.
// `payments`-gated doc line — see § Gated element ids live in gated DOC LINES.
#[cfg_attr(feature = "payments", doc = " The tail belongs to `post-tip-list`.")]
#[uniffi::export]
pub fn tip_more(n: i64) -> LocalizedText {
    fauna_core::format::tip_more(n)
}

/// `fauna_core::format::grace_countdown` → `Some(LocalizedText)` (keys
/// `time.countdown_dh` / `time.countdown_h`) while `deadline_ms > now_ms`;
/// `None` once elapsed — the caller renders its own already-localized "elapsed"
/// label (e.g. the mail primary-domain-rename banner's
/// `admin.dns.rename.grace_elapsed`). Unifies the byte-identical per-app
/// hand-rolls (web, linux, apple, android, windows all hardcoded the `d`/`h`
/// unit letters) onto one source of truth (priority #1/#2/#4). See
/// `docs/goal/behavior/value-formatting.md` § Grace countdown.
#[uniffi::export]
pub fn grace_countdown(deadline_ms: i64, now_ms: i64) -> Option<LocalizedText> {
    fauna_core::format::grace_countdown(deadline_ms, now_ms)
}

/// `fauna_core::format::url_host` → the display host of a `scheme://host[:port]/…`
/// URL (scheme/port/path dropped), e.g. `"example.com"` for
/// `"https://example.com/article"`; returns the original when no host can be
/// isolated. The shared label parse the link-preview `link-preview-domain` child
/// renders (render-model.md § D4) — unifies the per-app URL-host parses onto
/// one source of truth (priority #1/#2/#4).
#[uniffi::export]
pub fn url_host(url: String) -> String {
    fauna_core::format::url_host(&url)
}

/// `fauna_core::format::hex_full` → the full lowercase hex string of a **byte**
/// id (every byte as two zero-padded hex chars; empty → `""`). The canonical
/// display/copy form of an actor or member id shown as the fallback when no
/// handle is available — unifies the per-app byte→hex copies (linux
/// `.to_hex()`, android `HexUtil.bytesToHex`, windows
/// `Convert.ToHexString(..).ToLowerInvariant()`, web `bytesToHex`, apple
/// `hexString`) onto one source of truth (priority #1/#2/#4). The full-length
/// sibling of [`hex_short`] (in `identity`). See
/// `docs/goal/behavior/value-formatting.md` § Hex id display.
#[uniffi::export]
pub fn hex_full(bytes: Vec<u8>) -> String {
    fauna_core::format::hex_full(&bytes)
}

/// `fauna_core::format::confidence_percent` → the whole-percent display of a
/// moderation classifier's `confidence_per_mille` (`0..=1000`), rounded half-up
/// (`920 ‰ → 92 %`, `995 ‰ → 100 %`). The single rounding contract behind the
/// moderation-queue `confidence` column — unifies the per-app `(m + 5) / 10`
/// copies (Windows, apple; Linux calls the core fn directly) onto one source of
/// truth so no client silently drifts to truncation (priority #1/#4). See
/// `docs/goal/behavior/value-formatting.md` § Confidence percent and
/// `docs/goal/behavior/moderation.md` § Where logic lives.
#[uniffi::export]
pub fn confidence_percent(per_mille: u16) -> u32 {
    fauna_core::format::confidence_percent(per_mille)
}

/// `fauna_core::format::quota_fraction` → a `{used_bytes, max_bytes}` usage pair
/// reduced to a `0.0..=1.0` bar-fill fraction, guarded against `max_bytes <= 0`
/// (returns `0.0`) and clamped to `1.0` when over-quota. The single computation
/// behind every app's storage/quota progress bar — unifies the per-app
/// copies (windows, android, apple, web) onto one source of truth, closing a
/// real `NaN`-on-zero-quota bug web's hand-rolled version had (priority
/// #1/#2/#4). See [`quota_percent`] for the whole-percent sibling and
/// `docs/goal/behavior/value-formatting.md` § Quota fraction.
#[uniffi::export]
pub fn quota_fraction(used_bytes: i64, max_bytes: i64) -> f64 {
    fauna_core::format::quota_fraction(used_bytes, max_bytes)
}

/// `fauna_core::format::quota_percent` → the whole-percent sibling of
/// [`quota_fraction`], rounded to the nearest percent, for a text label rather
/// than a bar-fill value. See `docs/goal/behavior/value-formatting.md` § Quota
/// fraction.
#[uniffi::export]
pub fn quota_percent(used_bytes: i64, max_bytes: i64) -> u32 {
    fauna_core::format::quota_percent(used_bytes, max_bytes)
}

/// `fauna_core::format::parse_port` → a validated TCP port (`1..=65535`) parsed
/// from a user-entered string, or `None` when invalid (empty / non-numeric /
/// `0` / out of range). The single validator behind the admin CalDAV- and
/// serving-port fields — unifies the per-app range checks; clients render
/// their existing `*_PORT_INVALID` i18n string on `None` (priority #1/#2/#4).
/// See `docs/goal/behavior/value-formatting.md` § Port validation.
#[uniffi::export]
pub fn parse_port(input: String) -> Option<u16> {
    fauna_core::format::parse_port(&input)
}

/// `fauna_core::format::parse_cap` → a validated non-negative tier-cap `i64`
/// parsed from a user-entered `admin-settings-tier-cap-*` field (inbox / storage
/// / devices / blob-size / feeds), or `None` when empty / non-numeric /
/// fractional / out of `i64` range. Clients save as `parse_cap(text) ?? prev`, so
/// a stray edit keeps the persisted value (no silent zeroing); a negative value
/// clamps to `0`. The single validator behind every app's tier-cap save —
/// unifies the per-app parses (priority #1/#2/#4). See
/// `docs/goal/behavior/value-formatting.md` § Tier cap validation.
#[uniffi::export]
pub fn parse_cap(input: String) -> Option<i64> {
    fauna_core::format::parse_cap(&input)
}

/// `fauna_protocol::admin::DEFAULT_LAPSE_TIER` → the quota tier a lapsed member
/// degrades to when a membership designation never names one explicitly
/// (monetization.md § Pillar 4 — lapse is a reversible quota downgrade, never a
/// suspension). Surfaced so the non-Rust apps *render* the shared default
/// instead of hand-copying the literal: linux reads the constant directly, and
/// apple (`AdminTiersView.swift`) + android (`AdminSettingsScreen.kt`) both
/// call this export. A row's Save always sends its current selection
/// explicitly, so this is a display default, never a wire default — passing an
/// empty `lapse_tier` is what asks the nest to apply it.
#[uniffi::export]
pub fn default_lapse_tier() -> String {
    fauna_protocol::admin::DEFAULT_LAPSE_TIER.to_string()
}

/// `fauna_protocol::admin::default_invite_tier` → the quota tier a
/// newly-admitted member starts at when an invite/direct-admit request
/// doesn't name one explicitly. A DIFFERENT concept from [`default_lapse_tier`]
/// (a lapsed member's degrade-to tier, not a new member's start tier) — both
/// currently resolve to `"free"`, but they are independently configurable
/// business decisions, so this is its own export, not a reuse of that one.
/// Surfaced so apps render the shared default instead of hand-copying the
/// literal.
#[uniffi::export]
pub fn default_invite_tier() -> String {
    fauna_protocol::admin::default_invite_tier()
}

/// `fauna_core::format::total_pages` → the total page count for an admin list
/// paginated by (`page_size`, `total` items), ceil-divided and floored to `1`.
/// Paired with [`current_page`] for the `"{current} / {total}"` display every
/// admin pagination surface renders. See `docs/goal/behavior/value-formatting.md`
/// § Pagination.
#[uniffi::export]
pub fn total_pages(total: i64, page_size: i64) -> i64 {
    fauna_core::format::total_pages(total, page_size)
}

/// `fauna_core::format::current_page` → the 1-based current page number from a
/// 0-based `offset` into a list paginated by `page_size`. See [`total_pages`].
#[uniffi::export]
pub fn current_page(offset: i64, page_size: i64) -> i64 {
    fauna_core::format::current_page(offset, page_size)
}

/// `fauna_core::format::next_page_offset` → the offset one page forward, or
/// `None` at the last page. The stepper half of the [`total_pages`]/
/// [`current_page`] pagination-display harvest — see its doc comment for the
/// per-app hand-rolls this unifies.
#[uniffi::export]
pub fn next_page_offset(offset: i64, total: i64, page_size: i64) -> Option<i64> {
    fauna_core::format::next_page_offset(offset, total, page_size)
}

/// `fauna_core::format::prev_page_offset` → the offset one page back, or `None`
/// at page 1. See [`next_page_offset`].
#[uniffi::export]
pub fn prev_page_offset(offset: i64, page_size: i64) -> Option<i64> {
    fauna_core::format::prev_page_offset(offset, page_size)
}

/// `fauna_core::format::parse_count` → a validated non-negative `u32` parsed from a
/// user-entered admin-mail integer-knob field (outbound / alias / spam / auth /
/// submission / IMAP `admin-mail-*-input`), or `None` when empty / non-numeric /
/// negative / fractional / out of `u32` range. Clients save as
/// `parse_count(text) ?? prev`, so a stray edit keeps the persisted value (no silent
/// zeroing). The single validator behind every app's mail-knob save — unifies the
/// per-app parses (priority #1/#2/#4). See
/// `docs/goal/behavior/value-formatting.md` § Mail-knob validation.
#[uniffi::export]
pub fn parse_count(input: String) -> Option<u32> {
    fauna_core::format::parse_count(&input)
}

/// `fauna_core::format::parse_count_u64` → the `u64` sibling of [`parse_count`] for
/// the one admin-mail knob whose range exceeds `u32`: the IMAP per-mailbox storage
/// ceiling (`admin-mail-imap-storage-bytes-input` → `storage_bytes_default`). Same
/// trim / fallback-to-`prev` semantics, consumed as `parse_count_u64(text) ?? prev`.
/// See `docs/goal/behavior/value-formatting.md` § Mail-knob validation.
#[uniffi::export]
pub fn parse_count_u64(input: String) -> Option<u64> {
    fauna_core::format::parse_count_u64(&input)
}

/// `fauna_core::format::parse_count_i64` → the `i64` sibling of [`parse_count`] for the
/// one per-alias knob whose wire column is signed: the mail-alias `rate_limit_per_hour`
/// override (`mail-aliases-add-sheet-rate-per-hour-input` → `rate_limit_per_hour:
/// Option<i64>`; null = unlimited, `0` = block all). A non-negative cap in meaning despite
/// the signed column, so it yields a non-negative value — empty / non-numeric / **negative**
/// / fractional / out-of-`i64`-range input → `None` (the optional add-sheet field consumes
/// `None` as "no override / unlimited", not a fall-back-to-prev). Unifies the per-app
/// per-alias rate-cap parses (web `parseOptInt`, the natives' `long.TryParse` / `Int64` /
/// `toLongOrNull`) onto one source of truth (priority #1/#2/#4); the sibling
/// `spam_threshold_override` (`Option<u32>`) needs no new fn — it reuses [`parse_count`].
/// See `docs/goal/behavior/value-formatting.md` § Mail-knob validation.
#[uniffi::export]
pub fn parse_count_i64(input: String) -> Option<i64> {
    fauna_core::format::parse_count_i64(&input)
}

/// `fauna_core::format::parse_weight_permille` → the create-feed factor-weight editor's
/// decimal multiplier (`feed-factor-weight-input`, e.g. `"2.0"`) → the wire's signed
/// per-mille `FactorWeightInput.weight_permille` (content-moderation-and-ranking.md
/// § Composition). Unparseable / non-finite input falls back to the `1.0` baseline
/// (`1000`); a **negative** weight is a designed case (a strong-negative factor sinks an
/// item — feed.md § Frame reconciliation). Rounds **half-away-from-zero** like the sibling
/// [`probability_to_per_mille`], so the apps never disagree on a midpoint: a C# hand-roll
/// would round banker's, a JS one half-up. Unifies the linux / web hand-rolls (priority
/// #1/#2/#4). See `docs/goal/behavior/value-formatting.md` § Factor weight.
#[uniffi::export]
pub fn parse_weight_permille(input: String) -> i64 {
    fauna_core::format::parse_weight_permille(&input)
}

/// `fauna_core::format::format_weight_permille` — the inverse of
/// [`parse_weight_permille`]: a wire `weight_permille` → the create-feed factor
/// chip's display multiplier (`"{name} × {weight}"`). Rounds to 2 decimal places
/// and strips trailing zeros, so a whole multiplier reads `"1"`, never `"1.00"`.
/// Unifies the windows/apple hand-rolls (priority #1/#2/#4). See
/// `docs/goal/behavior/value-formatting.md` § Factor weight.
#[uniffi::export]
pub fn format_weight_permille(weight_permille: i64) -> String {
    fauna_core::format::format_weight_permille(weight_permille)
}

/// `fauna_protocol::spam::probability_to_per_mille` → convert a `0.0–1.0` spam/
/// phishing slider probability to the `0–1000` per-mille wire value (clamped +
/// rounded half-away-from-zero). Apple / Windows / Android call this instead of
/// each hand-rolling `* 1000`, so every app rounds identically — closing the
/// C# `Math.Round` banker's-rounding divergence the inline copies risked. The
/// *logic* lives in `fauna_protocol::spam` next to `spam_threshold_band`; its FFI
/// face lives here in the default-on `value-format` module (not the ungated
/// `spam` module) so the Go mail-bridge `--no-default-features` build drops it —
/// no `fauna-mail-go` binding churn. See `docs/goal/ui/settings.md` § Spam
/// threshold slider labels.
#[uniffi::export]
pub fn probability_to_per_mille(probability: f64) -> u16 {
    fauna_protocol::spam::probability_to_per_mille(probability)
}

/// `fauna_protocol::spam::per_mille_to_probability` → convert the `0–1000`
/// per-mille wire value back to a `0.0–1.0` slider probability (over-range
/// saturates at `1.0`). The inverse of [`probability_to_per_mille`]; same
/// module-placement rationale (Go-gating) as above.
#[uniffi::export]
pub fn per_mille_to_probability(per_mille: u16) -> f64 {
    fauna_protocol::spam::per_mille_to_probability(per_mille)
}

/// `fauna_core::format::conversation_timestamp_display` →
/// `{ clock, localized, absolute_epoch_ms }` (exactly one `Some`) for a
/// conversation/thread last-activity time, bucketed in the caller's local
/// timezone (`utc_offset_seconds`): render `clock` for today, `localized` for
/// Yesterday / a weekday, else format `absolute_epoch_ms` with the platform's
/// native locale-aware date formatter. See value-formatting.md § Conversation timestamp.
#[uniffi::export]
pub fn conversation_timestamp_display(
    now_ms: i64,
    then_ms: i64,
    utc_offset_seconds: i32,
) -> ConversationTimestampDisplay {
    fauna_core::format::conversation_timestamp_display(now_ms, then_ms, utc_offset_seconds)
}

/// `fauna_core::format::format_unix_local` — a fixed, non-localized
/// `"YYYY-MM-DD HH:MM"` local wall-clock render of a unix-seconds timestamp,
/// for audit/technical displays (credential created-at, nest-trust grant
/// history) that intentionally do not localize. Falls back to the raw number
/// on an ambiguous/out-of-range local offset. Native-only (`local-clock`,
/// forwarded by this crate's `value-format` feature) — needs the OS timezone
/// database, unavailable on wasm32. See value-formatting.md § Absolute local
/// timestamp display.
#[uniffi::export]
pub fn format_unix_local(secs: i64) -> String {
    fauna_core::format::format_unix_local(secs)
}

/// `fauna_core::format::format_unix_local_ms` — the epoch-milliseconds adapter
/// over [`format_unix_local`], flooring (`div_euclid`) rather than truncating
/// toward zero so a pre-1970 sub-second instant renders the earlier minute.
/// Exists so ms-valued callers (most FFI timestamps) never write their own
/// ms→secs chain with its own fallback. Same `local-clock` gating.
#[uniffi::export]
pub fn format_unix_local_ms(ms: i64) -> String {
    fauna_core::format::format_unix_local_ms(ms)
}

/// `fauna_core::format::format_unix_local_date` — the date-only sibling of
/// [`format_unix_local`]: a fixed, non-localized local `"YYYY-MM-DD"` for
/// compact date fields (cert expiry, mail-alias/list created-at columns).
/// `%Y-%m-%d` has no locale-varying component, which is what makes it shared
/// where a locale-aware short form stays platform-side. Same raw-number
/// fallback and `local-clock` gating as the datetime form. See
/// value-formatting.md § Absolute local timestamp display → the date-only
/// sibling.
#[uniffi::export]
pub fn format_unix_local_date(secs: i64) -> String {
    fauna_core::format::format_unix_local_date(secs)
}

/// `fauna_core::format::format_unix_local_date_ms` — the epoch-milliseconds
/// adapter over [`format_unix_local_date`] (floor, not truncate-toward-zero;
/// see that fn's doc for why the distinction is a calendar-day bug pre-1970).
#[uniffi::export]
pub fn format_unix_local_date_ms(ms: i64) -> String {
    fauna_core::format::format_unix_local_date_ms(ms)
}

/// `fauna_core::format::contact_status_label` → a `LocalizedText` `{ key, args }`
/// for a contact relationship status badge (`pending`/`accepted`/`confirmed`/
/// `blocked` → `common.*`, unknown → capitalized verbatim) the client resolves
/// through its i18n pipeline. Lifts the per-app status→label maps (android/
/// apple raw-capitalized, windows i18n-keyed but missing `confirmed`, web/linux
/// raw-lowercase) onto one source of truth (priority #1/#2/#4); the status
/// icon/color stays an idiomatic per-app render. See contacts.md § Where logic
/// lives → Status badge text.
#[uniffi::export]
pub fn contact_status_label(status: String) -> LocalizedText {
    fauna_core::format::contact_status_label(&status)
}

/// `fauna_core::format::thread_label_display` → a `LocalizedText` `{ key, args }`
/// for a conversation thread's display label: a blank/whitespace-only label carries
/// the canonical `conversations.detail.no_subject` key (`"(no subject)"`), a non-empty
/// label rides verbatim. Lifts the per-app empty-label fallback (windows/apple
/// hardcoded the untranslated literal `"(no label)"`; linux/web applied none) onto one
/// source of truth (priority #1/#2/#4). The raw label stays the filter/sort/rename value.
/// See conversations.md § Where logic lives → thread label display.
#[uniffi::export]
pub fn thread_label_display(label: String) -> LocalizedText {
    fauna_core::format::thread_label_display(&label)
}

/// `fauna_core::format::media_sort_label` → a `LocalizedText` `{ key, args }` for
/// the `media-sort-select` option label: the wire value (`"name"`/`"size"`/
/// `"date"`) maps to `media.sort_*`, unrecognized → `sort_name`. Lifts the
/// identical map every app hand-rolls onto one source of truth (priority
/// #2/#4). See media.md § Layout & flow.
#[uniffi::export]
pub fn media_sort_label(value: String) -> LocalizedText {
    fauna_core::format::media_sort_label(&value)
}

/// `fauna_core::format::media_sort_direction_label` → a `LocalizedText` for the
/// `media-sort-direction` option label: the `descending` flag maps to
/// `media.sort_ascending`/`sort_descending`.
#[uniffi::export]
pub fn media_sort_direction_label(descending: bool) -> LocalizedText {
    fauna_core::format::media_sort_direction_label(descending)
}

/// `fauna_core::format::share_link_expiry_label` → the `share_link.expiry_*`
/// label for a `share-link-expiry-select` option value (`"1d"` / `"7d"` /
/// `"30d"` / `"1y"`); `None` for a value outside the list, which the app
/// paints raw. The value stays the option's model key so the cross-app
/// `select(id, "<value>")` holds (`share-links.md` § Expiry).
#[uniffi::export]
pub fn share_link_expiry_label(value: String) -> Option<LocalizedText> {
    fauna_core::format::share_link_expiry_label(&value)
}

/// `fauna_core::format::share_link_state_label` → the `share_link.state_*`
/// label for a share-link row's stable state (`"active"` / `"expired"` /
/// `"revoked"`) — the `share-link-item-state` text, whose `state` attribute
/// keeps the stable value; `None` for an unknown state, painted raw
/// (`share-links.md` § Flows → List).
#[uniffi::export]
pub fn share_link_state_label(state: String) -> Option<LocalizedText> {
    fauna_core::format::share_link_state_label(&state)
}

/// `fauna_core::source_glyph::SourceGlyph::emoji` → the display emoji for a
/// source concept, for the conversations rail and the feed badge.
///
/// A plain `String`, **not** a `LocalizedText`: the glyph is a brand/concept
/// mark, identical in every locale. Lifts the identical six-case map all seven
/// apps hand-rolled onto one source of truth (priority #2/#4) — they had begun
/// to disagree on the envelope's emoji-presentation selector. See
/// `render-model.md` § Deltas → D5.
#[uniffi::export]
pub fn source_glyph_emoji(glyph: fauna_core::source_glyph::SourceGlyph) -> String {
    glyph.emoji().to_string()
}

/// `fauna_core::notification_glyph::NotificationGlyph::from_notif_type` → the
/// semantic category of a Notifications-page row, for a native app that picks its
/// own platform asset (`notifications.md` § Where logic lives: *shared Rust
/// returns the type enum; app glue picks the icon*).
///
/// Returns the ENUM, not an emoji, which is the difference from
/// [`source_glyph_emoji`] above: a source glyph is a brand mark every app paints
/// identically, while a notification icon is a platform-native asset — apple maps
/// this to SF Symbols, android to Material icons, and only linux and web want the
/// shared [`fauna_core::notification_glyph::NotificationGlyph::emoji`] family.
/// So the thing worth sharing is the classification of the `notif_type` wire
/// string, which every app would otherwise hand-roll (and two already had).
#[uniffi::export]
pub fn notification_glyph_for_type(
    notif_type: String,
) -> fauna_core::notification_glyph::NotificationGlyph {
    fauna_core::notification_glyph::NotificationGlyph::from_notif_type(&notif_type)
}

/// `fauna_core::format::os_maintenance_status_label` → a `LocalizedText` for the
/// `admin-nest` `nest-os-maintenance-status` line (reboot-pending / updates-pending /
/// up-to-date, from the `os_*` fields on `fauna.setup.status`). Lifts the state→key
/// decision all seven apps would otherwise hand-roll onto one source of truth
/// (priority #2, preventative — like `contact_status_label`). The raw count badge
/// (`nest-os-updates-count`) renders `os_security_updates_pending` directly. See
/// `installers/vps.md` § Host OS Maintenance § 4.
#[uniffi::export]
pub fn os_maintenance_status_label(
    security_updates_pending: u32,
    reboot_pending: bool,
) -> LocalizedText {
    fauna_core::format::os_maintenance_status_label(security_updates_pending, reboot_pending)
}

/// `fauna_core::format::mail_health_state_label` → a `LocalizedText` for the
/// `admin-mail-health-status` line from the `fauna.bridges.mail_health` reply's
/// open-enum `state` (unknown → the generic "needs attention" key). See
/// `mail-deliverability.md` § The mail health readout.
#[uniffi::export]
pub fn mail_health_state_label(state: String) -> LocalizedText {
    fauna_core::format::mail_health_state_label(&state)
}

/// `fauna_core::format::mail_health_check_state_label` → a `LocalizedText` for one
/// `admin-mail-health-check-state` (`pass` / `warn` / `fail` / `info`; unknown →
/// "needs attention"). See `mail-deliverability.md` § The mail health readout.
#[uniffi::export]
pub fn mail_health_check_state_label(state: String) -> LocalizedText {
    fauna_core::format::mail_health_check_state_label(&state)
}

/// `fauna_core::format::contact_matches_filter` → does a contact roster row match
/// the `contacts-search-field` query? Case-insensitive substring of the trimmed
/// query over `handle` / `domain` / hex `actor_id` (empty query matches all). Lifts
/// the per-app roster filter (web matched actor-id only, linux/windows
/// handle+actor-id, android handle+domain+actor-id, iOS/macOS had none) onto one
/// shared predicate over the enriched `FfiContactItem.handle`/`domain`. Local-only
/// — never a nest query. See contacts.md § Where logic lives → Contact roster filter.
#[uniffi::export]
pub fn contact_matches_filter(
    query: String,
    handle: Option<String>,
    domain: Option<String>,
    actor_id: String,
) -> bool {
    fauna_core::format::contact_matches_filter(
        &query,
        handle.as_deref(),
        domain.as_deref(),
        &actor_id,
        // No nickname, no labels: this free function has no projection to
        // read them from. The roster filter that matches them is
        // `FfiContactOverlays::matches_filter` (`crate::contact_overlays`),
        // which android and apple call; this export stays only until
        // windows moves to it with its overlay leg.
        None,
        &[],
    )
}

/// `fauna_core::format::contact_toggle_block_label` → a `LocalizedText` `{ key, args }`
/// for the `profile-block-button` toggle: already-blocked → `profile.unblock`, else
/// `profile.block`. Lifts the identical `is_blocked ? unblock : block` map all five
/// apps that surface the button hand-roll onto one source of truth (priority #2/#4 —
/// preventative, like `contact_status_label`); the button *style* stays per-app. See
/// profile.md § Element table and contacts.md § Where logic lives → Unblock.
#[uniffi::export]
pub fn contact_toggle_block_label(is_blocked: bool) -> LocalizedText {
    fauna_core::format::contact_toggle_block_label(is_blocked)
}

/// `fauna_core::format::follow_toggle_label` → a `LocalizedText` `{ key, args }` for
/// the `profile-follow-button`: already following → `profile.following`, else
/// `profile.follow`. Lifts the bool→2-key map the apps hand-rolled divergently
/// (web/android ternary, linux/windows on-success flip, apple always-"Follow") onto
/// one source of truth (priority #2/#4); the `is_following` derivation and the
/// followers-tier constant stay per-app by ratified decision. See profile.md
/// § Element table → `profile-follow-button`.
#[uniffi::export]
pub fn follow_toggle_label(is_following: bool) -> LocalizedText {
    fauna_core::format::follow_toggle_label(is_following)
}

/// `fauna_core::format::contact_row_blocks_actor` → does this contact roster row mean
/// the `target_actor_id` is blocked (`peer_id` matches the target — case-insensitive
/// hex — AND `status == "blocked"`)? Clients fold it over `fauna.contacts.list` with
/// `.any(..)` to derive the `profile-block-button` state. Lifts the per-app predicate
/// (windows case-insensitive, the other four case-sensitive) onto one shared rule
/// (priority #4). Local-only. See contacts.md § Where logic lives → Unblock.
#[uniffi::export]
pub fn contact_row_blocks_actor(
    row_peer_id: String,
    row_status: String,
    target_actor_id: String,
) -> bool {
    fauna_core::format::contact_row_blocks_actor(&row_peer_id, &row_status, &target_actor_id)
}

/// `fauna_core::format::device_status_label` → a `LocalizedText` `{ key, args }` for the
/// Devices page `device-status` label: `devices.online` when the device's `online` flag
/// is set, else `devices.offline`. Lifts the per-app online→label map (windows hardcoded
/// English; the others already i18n-keyed) onto one source of truth (priority #2/#4),
/// resolving the windows #1 violation; the status dot *color* stays per-app. See
/// devices.md § Where logic lives and § Element table (`device-status`).
#[uniffi::export]
pub fn device_status_label(online: bool) -> LocalizedText {
    fauna_core::format::device_status_label(online)
}

/// `fauna_core::format::device_place_label` → a `LocalizedText` `{ key, args }` for
/// the Devices page `device-folder-role-badge` chip: one device place's three flags
/// (`DeviceFolderRole`), composed from the wizard's `devices.wizard.place_*` labels.
/// The two- and three-flag templates carry KEYS as arguments, so render through the
/// app's nested resolve. devices.md § Element table (`device-folder-role-badge`).
#[uniffi::export]
pub fn device_place_label(originates: bool, accepts: bool, applies_deletes: bool) -> LocalizedText {
    fauna_core::format::device_place_label(originates, accepts, applies_deletes)
}

/// `fauna_core::format::mail_serving_status_label` → a `LocalizedText` `{ key, args }`
/// for the admin Users page `admin-users-mail-serving-status` read-only indicator:
/// `admin.users_page.serving_here` when the user's `mail_serving_enabled` flag is set,
/// else `admin.users_page.serving_disabled`. Lifts the per-app `enabled ?
/// serving_here : serving_disabled` map (all five apps hand-rolled it identically)
/// onto one source of truth (priority #2/#4); view-only, no wire change. See admin.md
/// § Users (`admin-users-mail-serving-status`) and value-formatting.md.
#[uniffi::export]
pub fn mail_serving_status_label(enabled: bool) -> LocalizedText {
    fauna_core::format::mail_serving_status_label(enabled)
}

/// `fauna_core::format::claim_status_label` → a `LocalizedText` `{ key, args }`
/// for the §5 manual-claims list's status badge:
/// redeemed / voided / unredeemed. Lifted before the pending per-app §5
/// lifts could hand-roll it five more times (priority #4 — resolve drift,
/// don't replicate it). **Redeemed wins over voided**: the two wire booleans
/// (`ClaimItem.redeemed_by` / `.voided_at`) are independent, so the branch
/// order is a real decision and belongs in one place. View-only, no wire
/// change. Lives in `value_format` so the Go mail-bridge's
/// `--no-default-features` build drops it (no binding churn). See
/// monetization.md § Pillar 3.
// `payments`-gated doc line — see § Gated element ids live in gated DOC LINES.
#[cfg_attr(
    feature = "payments",
    doc = " The badge is `subscription-claim-status[i]`."
)]
#[uniffi::export]
pub fn claim_status_label(redeemed: bool, voided: bool) -> LocalizedText {
    fauna_core::format::claim_status_label(redeemed, voided)
}

/// `fauna_core::format::provider_status_label` → a `LocalizedText` `{ key, args }`
/// for the §4 provider-row status badge: `configured` / `verified` / `error`
/// (monetization.md § Pillar 3 → "Provider status — evidence-based, no ping",
/// ratified 2026-07-16). Evidence-based, never an active probe — derives
/// purely from `ProviderItem.{last_verified_at,last_rejected_at}` (epoch
/// seconds; `None` = no evidence yet). One shared decision so no client
/// re-derives the branch, matching the `claim_status_label` precedent. Lives
/// in `value_format` so the Go mail-bridge's `--no-default-features` build
/// drops it (no binding churn).
#[uniffi::export]
pub fn provider_status_label(
    last_verified_at: Option<u64>,
    last_rejected_at: Option<u64>,
) -> LocalizedText {
    fauna_core::format::provider_status_label(last_verified_at, last_rejected_at)
}

/// `fauna_core::format::dns_verdict_label` → a `LocalizedText` `{ key, args }` for
/// the `admin-dns` record-matrix verdict label: the `fauna.dns.verify_records`
/// verdict's serde variant name (`Ok`/`Missing`/`Mismatch` → `admin.dns.status_*`,
/// anything else — `Checking`, absent, drift — → `status_checking`). Natives pass
/// their typed `VerifyStatus`'s variant name (or `""` for an absent verdict); the
/// verdict CSS class stays a per-app render. Lifts the identical verdict→key map
/// linux (`verdict_text_and_class`) / windows / web (`statusLabel`) hand-roll onto
/// one source of truth (priority #2/#4). See value-formatting.md § DNS verdict label.
///
/// `observed` is the row's `RecordVerdict.observed` — what public DNS actually
/// served — which a `Mismatch` renders into the label. Natives pass it straight
/// through; an absent verdict passes an empty list alongside the empty status.
#[uniffi::export]
pub fn dns_verdict_label(status: String, observed: Vec<String>) -> LocalizedText {
    fauna_core::format::dns_verdict_label(&status, &observed)
}

/// `fauna_core::format::cert_status_label` → a `LocalizedText` `{ key, args }` for
/// the `admin-dns` served-cert health badge's **state** word: the
/// `fauna.tls.cert_status` `CertHealthState`'s serde variant name (`ValidTrusted` →
/// `status_valid`, `Expiring` → `status_expiring`, else — `OnFloorRenewNeeded` /
/// drift — → `status_on_floor`). Only the state word — a badge wants
/// [`cert_status_view`], which composes this with the sub-label that follows it.
/// Lifts the identical state→key map linux (`cert_status_text`) / windows / web
/// (`certStatusText`) hand-roll onto one source of truth (priority #2/#4). See
/// value-formatting.md § Cert status badge and tls-certificates.md § C.4.
#[uniffi::export]
pub fn cert_status_label(state: String) -> LocalizedText {
    fauna_core::format::cert_status_label(&state)
}

/// `fauna_core::format::cert_status_view` → the whole `admin-dns` served-cert
/// badge: the state word plus which of the two mutually-exclusive sub-labels
/// follows it. Pass the `CertStatusRow`'s `state` variant name, `is_floor`, and
/// `not_after_unix`; render `"{admin.dns.cert.label} {state}"`, then
/// `({admin.dns.cert.self_signed})` when `show_self_signed`, or
/// `admin.dns.cert.expires{date}` when `expires_at_unix` is `Some` — formatting
/// that epoch with the platform's own locale-aware date formatter.
///
/// The two sub-labels are never both set: a floor cert's own (far-future) expiry
/// is withheld, because what the admin needs is a *trusted* cert. Lifts the
/// composition all five apps hand-rolled — and on which apple drifted into
/// showing a self-signed cert a reassuring 4096-01-01 expiry (priority #2/#4).
/// See value-formatting.md § Cert status badge and tls-certificates.md
/// § Where logic lives ("per-app shells … no cert logic of their own").
#[uniffi::export]
pub fn cert_status_view(state: String, is_floor: bool, not_after_unix: i64) -> CertStatusView {
    fauna_core::format::cert_status_view(&state, is_floor, not_after_unix)
}

/// `fauna_core::format::offer_status` → the per-tier [`OfferStatus`]
/// (`None`/`Pending`/`Active`, precedence Active>Pending>None) for the
/// `subscription-offers-section` badge on another's profile. `status_tier` is the
/// viewer's confirmed held tier (`status.get`); `pending` a transient post-click
/// flag (`status.get` carries no pending discriminant). The enum crosses directly
/// (`fauna-core/uniffi`) — natives branch on it for the badge label
/// ([`offer_status_label`]) **and** the Subscribe button state. Lifts the
/// android/apple/windows local enums + the web/linux inline derivations onto one
/// source of truth (priority #2/#4). See profile.md / monetization.md § Pillar 1.
#[uniffi::export]
pub fn offer_status(tier_name: String, status_tier: Option<String>, pending: bool) -> OfferStatus {
    fauna_core::format::offer_status(&tier_name, status_tier.as_deref(), pending)
}

/// `fauna_core::format::offer_status_label` → a `LocalizedText` `{ key, args }` for
/// the `subscription-offer-status` badge (`subscriptions.offer_status_*`) the client
/// resolves through its i18n pipeline. The status icon/color stays per-app.
#[uniffi::export]
pub fn offer_status_label(status: OfferStatus) -> LocalizedText {
    fauna_core::format::offer_status_label(status)
}

/// `fauna_core::format::backup_last_upload_label` → the
/// `backup-destination-last-upload-time` row text `{ label, when }`: resolve
/// `label` (`backups.backup_destination_last_upload[_never]`); when `when` is
/// present, resolve it first (a [`RelativeTimeDisplay`]) and substitute it as
/// the label's `{when}` arg. `last_upload_secs` is the status's raw
/// `last_upload_time` (unix **seconds**; `None`/`0` ⇒ "never" — the shared
/// guard). Lifts the never-vs-real decision + seconds→ms conversion the four
/// native apps hand-rolled (apple/android would render a zero timestamp as
/// the 1970 epoch) onto one source of truth (priority #1/#2/#4). See
/// value-formatting.md § Backup destination status labels.
#[uniffi::export]
pub fn backup_last_upload_label(
    last_upload_secs: Option<u64>,
    now_ms: i64,
) -> BackupLastUploadDisplay {
    fauna_core::format::backup_last_upload_label(last_upload_secs, now_ms)
}

/// `fauna_core::format::backup_last_audit_label` → the
/// `backup-destination-last-audit-time` row text `{ label, when }`, the audit
/// twin of [`backup_last_upload_label`] and resolved exactly like it: resolve
/// `label` (`backups.backup_destination_last_audit[_never]`); when `when` is
/// present, resolve it first (a [`RelativeTimeDisplay`]) and substitute it as the
/// label's `{when}` arg.
///
/// `last_passed_secs` is [`crate::FfiDestinationAuditRow::last_passed_at`] — unix
/// **seconds** of the last *passed* audit; `None`/`0` ⇒ "never", which is the
/// honest reading of a fresh enrollment and is deliberately **not** an alert.
/// Wrapped here rather than left to each shell for the same reason its upload
/// sibling was: the never-vs-real decision plus the seconds→ms conversion are
/// exactly what apple/android hand-rolled into a 1970-epoch render last time
/// (priority #1/#2/#4). See value-formatting.md § Backup destination status
/// labels and `docs/goal/ui/backups.md` § Audit-alert surface.
#[uniffi::export]
pub fn backup_last_audit_label(
    last_passed_secs: Option<u64>,
    now_ms: i64,
) -> fauna_core::format::BackupLastAuditDisplay {
    fauna_core::format::backup_last_audit_label(last_passed_secs, now_ms)
}

/// `fauna_core::format::backup_self_audit_label` → the
/// `backup-destination-last-audit-time` row text `{ label, when }` for a
/// **client-device custodian** row — a separate door from
/// [`backup_last_audit_label`] all the way down, because the owner-side loop
/// and a custodian's self-report answer the same question from opposite sides
/// of the trust line (`docs/goal/ui/backups.md` § Audit-alert surface → *The
/// client-device arm*).
///
/// `last_passed_secs` is [`crate::FfiBackupDestinationStatus::last_audit_passed_at`]
/// — unix **seconds** of the custodian's last *passed* self-audit; `None`/`0`
/// ⇒ "Self-checked: not yet", never a verdict.
#[uniffi::export]
pub fn backup_self_audit_label(
    last_passed_secs: Option<u64>,
    now_ms: i64,
) -> fauna_core::format::BackupLastAuditDisplay {
    fauna_core::format::backup_self_audit_label(last_passed_secs, now_ms)
}

/// `fauna_core::format::backup_self_audit_is_alerting` → whether a custodian's
/// reported `audit_state` ([`crate::FfiBackupDestinationStatus::audit_state`])
/// must raise a `backup-audit-alert` with `BackupAuditAlertReason::SelfReported`.
/// Absence and an unrecognised value both stay quiet — the single shared
/// answer, so no app can drift into alerting on a custodian that has simply
/// not audited yet.
#[uniffi::export]
pub fn backup_self_audit_is_alerting(audit_state: Option<String>) -> bool {
    fauna_core::format::backup_self_audit_is_alerting(audit_state.as_deref())
}

/// `fauna_core::format::backup_audit_alert_label` → the complete `LocalizedText`
/// for one `backup-audit-alert` banner, naming **both** the destination and the
/// reason (the banner is indexed — one per failing destination — so a bare
/// "backup problem" would not say which one).
///
/// `reason` is a non-`None` [`crate::FfiDestinationAuditRow::alert_reason`]; a
/// row whose reason is `None` renders no banner at all. `destination_label` is
/// the same string the status row shows — [`crate::backup_destination_label`].
/// The lag/overdue durations render as whole floored days inside the shared
/// formatter, so no shell computes a duration.
#[uniffi::export]
pub fn backup_audit_alert_label(
    reason: fauna_core::format::BackupAuditAlertReason,
    destination_label: String,
) -> LocalizedText {
    fauna_core::format::backup_audit_alert_label(reason, &destination_label)
}

/// `fauna_core::format::backup_backlog_label` → a complete `LocalizedText`
/// (`backups.backup_destination_backlog` + `{count}`) for the
/// `backup-destination-backlog-count` row text; `None` (no status read yet)
/// carries the shared 0 baseline. See value-formatting.md § Backup destination
/// status labels.
#[uniffi::export]
pub fn backup_backlog_label(backlog_count: Option<u32>) -> LocalizedText {
    fauna_core::format::backup_backlog_label(backlog_count)
}

/// `fauna_core::format::backup_destination_kind_label` → the
/// `backup-destination-kind-badge` text for one destination row: which kind of
/// destination this is (`backups.backup_destination_kind_*`).
///
/// `kind` is [`crate::FfiBackupDestinationView::kind`] — the raw discriminator,
/// handed straight back. **An unrecognised kind renders as itself** (the raw
/// string interpolated) rather than collapsing into a generic word: a row a
/// newer client wrote is precisely the case where the user needs to see *what*
/// their older build cannot drive. See `docs/goal/behavior/backup-destinations.md` § Third
/// destination kind → *Durability + labeling*.
#[uniffi::export]
pub fn backup_destination_kind_label(kind: String) -> LocalizedText {
    fauna_core::format::backup_destination_kind_label(&kind)
}

/// `fauna_core::format::backup_destination_kind_options` → the
/// `backup-destination-kind-select` catalog: the implemented kinds in paint
/// order (nest first — it is the kind that actually satisfies "off-site"), each
/// carrying the wire `value` the row records and the `label` to resolve.
///
/// Shared for the reason every picker catalog is, plus one specific to this
/// select: **the option a user picks and the badge they get back must be the
/// same text**, and seven apps each pairing a hand-written option list against
/// [`backup_destination_kind_label`] is seven chances for those to drift. The
/// ratified-but-deferred S3 kind is **absent** rather than present-and-disabled.
#[uniffi::export]
pub fn backup_destination_kind_options() -> Vec<fauna_core::format::BackupDestinationKindOption> {
    fauna_core::format::backup_destination_kind_options()
}

/// The `client-device` wire discriminator itself
/// (`fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE`), so a native shell can
/// tell **which arm of [`backup_destination_kind_options`] the user picked**
/// without privately mirroring the string.
///
/// This exists because the add dialog's question is genuinely not the status
/// row's. A row asks *"is this destination one of my devices?"*, which is
/// `every_destination_is_a_client_device`'s typed answer over a whole row (its
/// `Inert` arm needs the device id). A **select** asks only *"does the kind the
/// user just picked mean this device?"* — a pure discriminator comparison, with
/// no row to project yet — and a shell answering it by hard-coding
/// `"client-device"` has minted exactly the private mirror of a shared constant
/// that the `default_lapse_tier()` export exists to delete elsewhere. The
/// consequence of drifting is not cosmetic: the shell would paint the URL box
/// for a kind that has no address, and submit through the nest path, which
/// resolves an empty URL over the network.
///
/// Linux and tui need no such accessor — they link `fauna_core` and name the
/// constant directly. Web needs none either: its Backups page renders these rows
/// but deliberately offers no kind select (a browser cannot host the sealed
/// store the kind exists to provide), so it never has a selected kind to test.
#[uniffi::export]
pub fn destination_kind_client_device() -> String {
    fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.to_string()
}

/// `fauna_core::format::backup_usage_label` → the `backup-destination-usage`
/// row text (client-device rows only): held bytes against the user-set cap.
///
/// Split for client-side i18n exactly like [`backup_last_upload_label`]: resolve
/// `label`, and when `held` / `cap` are present resolve each first (they are
/// themselves `LocalizedText` byte sizes) and substitute them as the label's
/// `{held}` / `{cap}` args.
///
/// ⚠ **`cap_state` is read, never inferred.** Pass
/// [`crate::FfiBackupDestinationStatus`]'s `cap_state` through; do **not**
/// re-derive cap-reached from `held >= cap`. A pull pass that stopped at its cap
/// ends *below* the cap (a segment larger than the remaining headroom stops the
/// pass without filling it), so a shell inferring the verdict from the two
/// numbers renders "healthy, with room to spare" for a backup that has silently
/// stopped advancing — the exact failure `backups.md` § Third destination kind
/// requires be shown distinctly from ordinary lag.
///
/// `held_bytes: None` is "this custodian has never checked in", which reads as
/// *nothing held yet* rather than *0 bytes held*.
#[uniffi::export]
pub fn backup_usage_label(
    held_bytes: Option<u64>,
    capacity_cap_bytes: Option<u64>,
    cap_state: Option<String>,
) -> fauna_core::format::BackupUsageDisplay {
    fauna_core::format::backup_usage_label(held_bytes, capacity_cap_bytes, cap_state.as_deref())
}

/// `fauna_core::format::orphaned_store_label` → the `backup-orphaned-store-row`
/// sentence: what this device is still holding with no destination row to
/// justify it.
///
/// Split two-level for the same reason [`backup_usage_label`] is — resolve
/// `label`, then substitute the separately-resolved `held` into its `{held}`
/// slot, so the byte-size unit localizes before it lands inside the sentence.
///
/// ⚠ **This formatter does not decide whether the row paints.** That is
/// [`custodian_store_is_orphaned`], and keeping the two apart is deliberate: a
/// formatter that also gated the row would be a policy answer wearing a label's
/// clothes, and the policy here arms a **destructive** gesture. Call the
/// predicate, then call this for the text.
#[uniffi::export]
pub fn orphaned_store_label(held_bytes: u64) -> fauna_core::format::OrphanedStoreDisplay {
    fauna_core::format::orphaned_store_label(held_bytes)
}

/// `fauna_core::format::parse_byte_size` → the inverse of `byte_size`, and the
/// one the `backup-destination-capacity-input` cap is read through on every app.
///
/// Liberal about what a person types (`"50 GB"`, `"50GB"`, `"1,5 TB"`, a bare
/// `"1024"`) and strict about what counts as a number; units are **1024-based**,
/// matching `byte_size`'s own scaling, so a cap round-trips through the two
/// unchanged instead of drifting every time the page repaints it.
///
/// `None` is a **refusal the shell must surface**, never a substituted default:
/// silently recording a cap the user did not choose is exactly the class of
/// guess that fills a device's disk.
#[uniffi::export]
pub fn parse_byte_size(input: String) -> Option<u64> {
    fauna_core::format::parse_byte_size(&input)
}

/// `fauna_core::format::sync_display_state_label` → a `LocalizedText`
/// (`media.status_label.*`) for the per-file `sync-state-badge` (file-sync.md
/// § Per-file sync-status display). The desktop-engine clients map their local
/// per-file state to [`SyncDisplayState`] and resolve the label here; the badge
/// icon/color stays an idiomatic per-app render. [`SyncDisplayState`] crosses
/// the boundary directly (a `fauna_core` `uniffi::Enum`, like [`OfferStatus`]).
#[uniffi::export]
pub fn sync_display_state_label(state: SyncDisplayState) -> LocalizedText {
    fauna_core::format::sync_display_state_label(state)
}

/// `fauna_core::format::publish_kind_options` → the
/// `personalization-trained-factor-publish-kind-select` catalog: List | Model,
/// List first (the weaker disclosure, so it is what an unattended default
/// picks). The `backup_destination_kind_options` shape verbatim — the option a
/// user picks and the badge/label they read back must be the same text, and a
/// per-app hand-written two-item list is a chance for those to drift
/// (`docs/goal/behavior/topic-factors.md` § Publishing a trained factor).
#[uniffi::export]
pub fn publish_kind_options() -> Vec<fauna_core::format::PublishKindOption> {
    fauna_core::format::publish_kind_options()
}

/// `fauna_core::format::ngram_direction_label` → the Model review row's class-
/// direction text (`personalization-trained-factor-publish-ngram-direction`,
/// and the subscriber-side `labeler-inspect-model-entry-direction` twin) — the
/// dislike half is part of the disclosure, so a row dominated by *less like
/// this* examples must say so in words.
#[uniffi::export]
pub fn ngram_direction_label(more: u32, less: u32) -> LocalizedText {
    fauna_core::format::ngram_direction_label(more, less)
}

/// `fauna_core::format::ngram_doc_count_label` → the Model review row's
/// class-blind distinct-document count (`personalization-trained-factor-publish-ngram-count`,
/// and the subscriber-side `labeler-inspect-model-entry-count` twin) — `more +
/// less`, the quantity `TEXT_MODEL_PUBLISH_MIN_DOCS` bounds.
#[uniffi::export]
pub fn ngram_doc_count_label(more: u32, less: u32) -> LocalizedText {
    fauna_core::format::ngram_doc_count_label(more, less)
}

/// `fauna_core::format::text_model_needs_newer_app` → the
/// `labeler-catalog-item-kind` badge's override text, or `None` to paint the
/// raw `artifact_kind` discriminator unchanged (content-moderation-and-ranking.md
/// § Tier-3 artifact kinds, the unknown-version contract's "says so" half).
/// `artifact_kind`/`artifact_version` are `LabelerCatalogEntry`'s own fields,
/// handed straight back — never re-derived client-side.
#[uniffi::export]
pub fn text_model_needs_newer_app(
    artifact_kind: String,
    artifact_version: u64,
) -> Option<LocalizedText> {
    fauna_core::format::text_model_needs_newer_app(&artifact_kind, artifact_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ **The rule most likely to be lost in translation across a boundary.**
    /// `backup_usage_label` must read cap-reached from `cap_state` and never
    /// infer it from `held >= cap`: a pull pass that stops at its cap ends
    /// *below* the cap (a segment larger than the remaining headroom stops the
    /// pass without filling it), so an inferring shell renders a stalled backup
    /// as "healthy, with room to spare". The inversion is mutation-pinned
    /// nest-side in `fauna-sync-engine`'s pull tests; this pins that the
    /// exported face still carries it to the four UniFFI shells.
    #[test]
    fn exported_usage_label_reads_cap_reached_from_cap_state_not_from_the_numbers() {
        let held = 10u64 << 30;
        let cap = 50u64 << 30;

        // Well below the cap, but the pass reported it stopped there. The
        // sentinel comes from the shared constant, not a literal — a shell
        // spelling it itself is the same class of drift this face prevents.
        let stopped = backup_usage_label(
            Some(held),
            Some(cap),
            Some(fauna_core::data::CAP_STATE_REACHED.into()),
        );
        assert_eq!(
            stopped.label.key,
            "backups.backup_destination_usage_cap_reached"
        );

        // The same two numbers, healthy state — proving the verdict rides
        // `cap_state` alone.
        let healthy = backup_usage_label(
            Some(held),
            Some(cap),
            Some(fauna_core::data::CAP_STATE_OK.into()),
        );
        assert_eq!(healthy.label.key, "backups.backup_destination_usage");

        // Uncapped is a real configuration ("fill the disk"), not a zero cap.
        let uncapped = backup_usage_label(
            Some(held),
            None,
            Some(fauna_core::data::CAP_STATE_OK.into()),
        );
        assert_eq!(
            uncapped.label.key,
            "backups.backup_destination_usage_uncapped"
        );
        assert!(uncapped.cap.is_none());

        // Never checked in reads as *nothing held yet*, not *0 bytes held*.
        let silent = backup_usage_label(None, Some(cap), None);
        assert_eq!(silent.label.key, "backups.backup_destination_usage_unknown");
        assert!(silent.held.is_none());
    }

    /// The exported discriminator must actually **name one of the catalog's own
    /// options**, or a shell comparing the two never takes the custodian branch
    /// — it would paint the URL box for a kind with no address and submit
    /// through the nest path, resolving an empty URL over the network. The
    /// failure is silent at the boundary and only visible as a broken dialog, so
    /// the accessor and the catalog are pinned against each other here rather
    /// than each being asserted against a literal.
    #[test]
    fn the_exported_client_device_discriminator_names_a_real_catalog_option() {
        let client_device = destination_kind_client_device();
        let options = backup_destination_kind_options();
        assert!(
            options.iter().any(|o| o.value == client_device),
            "the exported client-device discriminator {client_device:?} matches no option in {:?}",
            options.iter().map(|o| &o.value).collect::<Vec<_>>(),
        );
        // And it is not the kind the catalog paints first — nest leads, because
        // it is the kind that actually satisfies "off-site".
        assert_ne!(
            options.first().map(|o| o.value.as_str()),
            Some(client_device.as_str())
        );
    }

    #[test]
    fn exported_kind_options_paint_the_badge_text_their_own_rows_carry() {
        let options = backup_destination_kind_options();
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, vec!["nest", "client-device"]);
        for option in &options {
            assert_eq!(
                option.label,
                backup_destination_kind_label(option.value.clone()),
                "option {:?} must paint the badge text its own row will carry",
                option.value
            );
        }
    }

    #[test]
    fn an_unrecognised_kind_renders_as_itself_over_the_boundary() {
        // A row a newer client wrote is exactly when the user needs to see
        // *what* their older build cannot drive.
        let label = backup_destination_kind_label("s3".into());
        assert_eq!(label.key, "backups.backup_destination_kind_unknown");
        assert_eq!(label.args.get("kind").map(String::as_str), Some("s3"));
    }

    #[test]
    fn exported_parse_byte_size_round_trips_a_cap_and_refuses_junk() {
        // 1024-based, so a cap survives a repaint through `byte_size`.
        assert_eq!(parse_byte_size("50 GB".into()), Some(50 << 30));
        assert_eq!(parse_byte_size("50gb".into()), Some(50 << 30));
        assert_eq!(parse_byte_size("1024".into()), Some(1024));
        // A refusal the shell surfaces, never a substituted default.
        assert_eq!(parse_byte_size("".into()), None);
        assert_eq!(parse_byte_size("lots".into()), None);
        assert_eq!(parse_byte_size("-5 GB".into()), None);
        assert_eq!(parse_byte_size("5 bananas".into()), None);
    }

    #[test]
    fn exported_publish_kind_options_are_list_then_model() {
        let options = publish_kind_options();
        let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, vec!["list", "text-model"]);
    }

    #[test]
    fn exported_ngram_direction_label_reads_the_dominant_class() {
        let more = ngram_direction_label(5, 1);
        assert_eq!(more.key, "personalization.publish_ngram_direction_more");
        let less = ngram_direction_label(1, 5);
        assert_eq!(less.key, "personalization.publish_ngram_direction_less");
        let tie = ngram_direction_label(3, 3);
        assert_eq!(tie.key, "personalization.publish_ngram_direction_both");
    }

    #[test]
    fn exported_ngram_doc_count_label_is_class_blind() {
        let label = ngram_doc_count_label(2, 3);
        assert_eq!(label.args.get("count").map(String::as_str), Some("5"));
    }

    #[test]
    fn exported_text_model_needs_newer_app_only_fires_on_a_claimed_unsupported_version() {
        // Absent (version 0) — a non-text-model artifact's non-claim, never "too new".
        assert_eq!(text_model_needs_newer_app("text-model".into(), 0), None);
        // A version this build implements.
        assert_eq!(
            text_model_needs_newer_app(
                "text-model".into(),
                fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION as u64
            ),
            None
        );
        // A claimed future contract this build cannot score.
        let badge = text_model_needs_newer_app(
            "text-model".into(),
            fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION as u64 + 1,
        );
        assert_eq!(
            badge.map(|t| t.key),
            Some("labeler_catalog.kind_needs_newer_app".to_string())
        );
        // Not a text-model at all — the badge never substitutes for a list/wasm row.
        assert_eq!(text_model_needs_newer_app("list".into(), 99), None);
    }
}
