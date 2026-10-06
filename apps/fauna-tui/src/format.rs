//! Value formatting for painted text — the tui's resolution of the **shared**
//! `fauna_core::format` decisions against its own string table.
//!
//! Every rule here (the 1024-unit byte scale, the relative-time bucketing) is
//! shared Rust and stays there (`behavior/value-formatting.md`, priority #2);
//! this module exists only because those helpers hand back a `LocalizedText`
//! decision that each app resolves with its own lookup. It is the tui-wide
//! home for that one-line resolution, so a page never grows its own copy — the
//! shape these helpers had before, when the same two functions lived privately
//! in `settings/root.rs` and `notifications.rs` and a third page needed them.
//!
//! The two-level resolves (an outer label whose `{when}` / `{held}` / `{cap}`
//! slot takes an inner text) are shared Rust too as of 2026-08-19 — they were
//! byte-identical here and in `fauna-linux`'s `i18n`, so they moved into
//! `fauna_core::format`'s `*_text` doors, which take the lookup as a parameter
//! the way `resolve_option_labels` already did. What stays here is the lookup
//! and the tui's own input adapters (micros, the unset-`0` sentinel).

/// A byte count on the shared 1024-unit scale (`media-item-size`,
/// `file-version-size`, the Settings quota cells).
///
/// Negative inputs clamp to zero — the wire's `i64` sizes are non-negative by
/// construction, and a display string is the wrong place to surface a violation.
pub fn byte_size(bytes: i64) -> String {
    fauna_core::format::byte_size(bytes.max(0) as u64).resolve(fauna_i18n::strings::lookup)
}

/// A tip amount on the shared sats/msats scale (`post-tip-total`, and each
/// `post-tip-item`'s amount) — `monetization.md` § Tips.
///
/// The unit split (sats above 1 sat, msats below) is a shared decision, exactly
/// like the byte scale above; this only resolves it against the tui's table.
#[cfg(feature = "payments")]
pub fn tip_amount(msats: i64) -> String {
    fauna_core::format::tip_amount(msats).resolve(fauna_i18n::strings::lookup)
}

/// A tip count with its singular (`post-tip-count`) — shared so no client
/// ships "1 tips".
#[cfg(feature = "payments")]
pub fn tip_count(count: i64) -> String {
    fauna_core::format::tip_count(count).resolve(fauna_i18n::strings::lookup)
}

/// The `post-tip-list` tail, "and N more", for a bounded attribution window.
///
/// `n` comes from the nest's own `has_more` + totals, never from comparing the
/// rendered row count against a cap this client hard-codes.
#[cfg(feature = "payments")]
pub fn tip_more(n: i64) -> String {
    fauna_core::format::tip_more(n).resolve(fauna_i18n::strings::lookup)
}

/// An epoch-**micro**second timestamp as display text, via the shared
/// relative-time bucketing — the linux `client.rs::format_epoch_us` twin.
///
/// `0` renders empty (an unset timestamp is not "1970") — the one decision this
/// door still owns, because only the caller knows whether its field uses `0` as
/// a sentinel. The rest is [`fauna_core::format::relative_time_text`]: buckets
/// inside the relative window ("2h ago") resolve off the shared string table;
/// anything older falls back to the shared plain local date, because a terminal
/// has no locale-aware date widget and the shared conversations bucketing
/// already renders absolutes that way.
pub fn format_epoch_us(us: i64) -> String {
    if us == 0 {
        return String::new();
    }
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    fauna_core::format::relative_time_text(now_ms, us / 1_000, fauna_i18n::strings::lookup)
}

/// A **future** epoch-*second* timestamp as a local `YYYY-MM-DD` date.
///
/// Deliberately not [`format_epoch_us`]: the shared relative-time bucketing
/// clamps any future `then` to "just now" (`fauna_core::format::relative_time`),
/// which is right for the past-event fields it was built for and useless for an
/// expiry. Bunker connections lapse up to 90 days out (`ui/nostr.md` § The nest
/// as the user's NIP-46 signer), so the honest render is the absolute date —
/// now literally the same shared call linux's `i18n::local_date` makes for the
/// same field, where this used to be a private chrono chain claiming to be.
pub fn epoch_secs_date(unix_secs: u64) -> String {
    fauna_core::format::format_unix_local_date(unix_secs as i64)
}

/// The `backup-destination-kind-badge` text, via the shared
/// `fauna_core::format::backup_destination_kind_label`. One level, so a plain
/// resolve — the unknown-kind arm carries its `{kind}` as an ordinary arg.
pub fn backup_destination_kind(kind: &str) -> String {
    fauna_core::format::backup_destination_kind_label(kind).resolve(fauna_i18n::strings::lookup)
}

/// The `personalization-trained-factor-publish-kind-select` option text, via
/// the shared `fauna_core::format::publish_kind_label`. Paint-only: the select
/// round-trips the wire discriminator, so this never becomes a driver contract.
pub fn publish_kind(kind: &str) -> String {
    fauna_core::format::publish_kind_label(kind).resolve(fauna_i18n::strings::lookup)
}

/// The Model review row's class-direction word, via the shared
/// `fauna_core::format::ngram_direction_label` — the same face the subscriber's
/// `labeler-inspect-model-entry-direction` reads, so the publisher's review and
/// what a subscriber sees cannot disagree.
pub fn ngram_direction(more: u32, less: u32) -> String {
    fauna_core::format::ngram_direction_label(more, less).resolve(fauna_i18n::strings::lookup)
}

/// The Model review row's class-blind distinct-post count, via the shared
/// `fauna_core::format::ngram_doc_count_label`.
pub fn ngram_doc_count(more: u32, less: u32) -> String {
    fauna_core::format::ngram_doc_count_label(more, less).resolve(fauna_i18n::strings::lookup)
}

/// The `media-sort-select` option label, via the shared
/// `fauna_core::format::media_sort_label` value → key decision (priority #2).
pub fn media_sort_label(value: &str) -> String {
    fauna_core::format::media_sort_label(value).resolve(fauna_i18n::strings::lookup)
}

/// The `media-sort-direction` option label, via the shared
/// `fauna_core::format::media_sort_direction_label` bool → key decision.
pub fn media_sort_direction_label(descending: bool) -> String {
    fauna_core::format::media_sort_direction_label(descending).resolve(fauna_i18n::strings::lookup)
}

/// The `filter-action` list-row badge label, via the shared
/// `fauna_protocol::email::filter_action_label` variant → key decision
/// (priority #2/#4). Unlike `describe_filter_action` (`None` for the richer
/// variants no form collects, which only gates *editability*), this always
/// resolves.
pub fn filter_action_label(action: &fauna_client_email::email::EmailFilterAction) -> String {
    fauna_client_email::email::filter_action_label(action).resolve(fauna_i18n::strings::lookup)
}

/// The `backup-destination-usage` row text, via the shared
/// [`fauna_core::format::backup_usage_text`].
///
/// Two-level like
/// [`fauna_client_backup::row_text::destination_last_upload_text`], but with
/// **two** inner texts rather than one: both byte sizes are themselves
/// `LocalizedText` (unit key + value),
/// so each is resolved before substitution — shared-side, and only into the
/// slots the label actually names. The cap-reached decision is *not* made here
/// either — the shared fn reads `cap_state` and never infers it from
/// `held >= cap`, which is the whole point of that field existing.
pub fn backup_usage(
    held_bytes: Option<u64>,
    capacity_cap_bytes: Option<u64>,
    cap_state: Option<&str>,
) -> String {
    fauna_core::format::backup_usage_text(
        held_bytes,
        capacity_cap_bytes,
        cap_state,
        fauna_i18n::strings::lookup,
    )
}

/// The `backup-orphaned-store-row` text, via the shared
/// [`fauna_core::format::orphaned_store_text`] — two-level like
/// [`backup_usage`], because the byte size is itself a `LocalizedText`.
pub fn orphaned_store(held_bytes: u64) -> String {
    fauna_core::format::orphaned_store_text(held_bytes, fauna_i18n::strings::lookup)
}

/// One `backup-audit-alert` banner's text, via the shared
/// `fauna_core::format::backup_audit_alert_label` — a complete sentence naming
/// both the destination and the reason, since the banner is indexed and a bare
/// "backup problem" would not say which one. One level, so a plain resolve.
pub fn backup_audit_alert(
    reason: fauna_core::format::BackupAuditAlertReason,
    destination_label: &str,
) -> String {
    fauna_core::format::backup_audit_alert_label(reason, destination_label)
        .resolve(fauna_i18n::strings::lookup)
}

// ── Calendar month / weekday names ──────────────────────────────────────────
//
// `ui/events.md` § Where logic lives: the portable Gregorian math lives in
// `fauna_core::caltime`, and the localized month/weekday *name* strings come
// from the i18n catalog rather than a platform date library — "referenced
// directly by app code, not via a fauna_core LocalizedText key".
//
// The four lookups themselves now live in `fauna_i18n::time`, beside the table
// they read (2026-08-19): linux's `views/events/time_utils.rs` carried a
// byte-for-byte identical set over the *same* constants, so they were one
// function duplicated, not two apps' glue. They are re-exported here so the
// tui-wide `crate::format::{month_name, …}` call-site path is unchanged.
//
// The three calendar *label* formatters that consume them stay app-side, in
// `events/grids.rs` — they need `caltime`, and `fauna-i18n` is a dependency-free
// leaf half the workspace links (see that crate's module docs).
pub use fauna_i18n::time::{month_name, month_name_short, weekday_name, weekday_short};

#[cfg(test)]
mod tests {
    use super::*;

    /// A future expiry renders as an absolute date, never a relative bucket —
    /// the whole reason this helper exists beside `format_epoch_us`.
    #[test]
    fn epoch_secs_date_renders_a_plausible_future_calendar_date() {
        // 2026-07-22T00:00:00Z. Rendered in local time, so assert the shape and
        // the year rather than an exact day the timezone could shift.
        let got = epoch_secs_date(1_784_678_400);
        assert!(got.starts_with("2026-07-"), "got {got}");
        assert_eq!(got.len(), 10, "YYYY-MM-DD");
    }

    /// The byte scale is the shared one, resolved against the tui's table — and
    /// a negative size clamps rather than wrapping into a huge `u64`.
    #[test]
    fn byte_size_uses_the_shared_scale_and_clamps_negatives() {
        assert_eq!(byte_size(0), byte_size(0));
        assert!(
            byte_size(2048).contains('2'),
            "2048 B should read as ~2 KiB"
        );
        // The real point: `as u64` on a negative would wrap to ~16 EiB.
        assert_eq!(byte_size(-1), byte_size(0));
    }

    /// An unset timestamp paints empty, not an epoch date.
    #[test]
    fn zero_epoch_renders_empty() {
        assert_eq!(format_epoch_us(0), "");
    }

    /// No painted text may take a month or weekday **name** from chrono.
    ///
    /// `ui/events.md` § Where logic lives puts those names in the client's i18n
    /// catalog, and `en.yaml` carries all 38 of them for that reason. A chrono
    /// name specifier bakes English into the binary *outside* the catalog — so
    /// the day the catalog gains a locale, every such site keeps rendering
    /// English and nothing points at it. This shipped: `events/grids.rs`
    /// resolved its month-grid weekday **header** off the catalog while the
    /// three date labels beside it, and the two column headers under them, went
    /// through `%B`/`%b`/`%A`/`%a` — one file, both ways, for months. Linux's
    /// twin had used its catalog throughout.
    ///
    /// The guard walks the tree rather than listing the known sites: the copy
    /// worth catching is the one nobody has written yet. Numeric specifiers are
    /// untouched *by this guard* — they carry no language, and a terminal has no
    /// locale-aware date widget to defer to. They are not unguarded, though:
    /// [`no_painted_text_hand_rolls_a_shared_owned_local_date`] below owns them
    /// on the separate duplication axis, and the four numeric sites this comment
    /// used to point at in this very file were exactly what it found.
    ///
    /// Note this test names no specifier literally; it assembles each needle
    /// from its letter. That is what lets it walk **its own file** with no
    /// exemption — an allow-list entry here would be a hole in exactly the
    /// module a future session would add a formatting helper to.
    #[test]
    fn no_painted_text_takes_a_month_or_weekday_name_from_chrono() {
        /// Chrono's locale-name specifiers, by letter and what each renders.
        const NAME_SPECIFIERS: &[(char, &str)] = &[
            ('A', "full weekday name"),
            ('a', "abbreviated weekday name"),
            ('B', "full month name"),
            ('b', "abbreviated month name"),
            ('h', "abbreviated month name (alias of the b form)"),
        ];

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read source");
                // Collapse whitespace first, so a rustfmt-wrapped
                // `.format(\n    "%b %-d",\n)` reads as one span — the exact
                // wrapping that hid eight sites from two hand sweeps of the
                // nest's own tree-walking guard in `state.rs`.
                let flat: String = text.split_whitespace().collect::<Vec<_>>().join("");
                let rel = path
                    .strip_prefix(&src)
                    .unwrap_or(&path)
                    .to_str()
                    .expect("utf-8 path")
                    .replace('\\', "/");
                // Only the inside of a `.format("…")` call counts: a bare `%b`
                // elsewhere is a SQL `LIKE` pattern or percent-encoding, not a
                // rendered name.
                for span in flat.split(".format(\"").skip(1) {
                    let Some((fmt, _)) = span.split_once('"') else {
                        continue;
                    };
                    for (letter, renders) in NAME_SPECIFIERS {
                        if fmt.contains(&format!("%{letter}")) {
                            offenders.push(format!(
                                "{rel}: .format(\"{fmt}\") renders the {renders} via chrono"
                            ));
                        }
                    }
                }
            }
        }
        offenders.sort();
        assert!(
            offenders.is_empty(),
            "month/weekday names must resolve off the i18n catalog \
             (`crate::format::{{month_name, month_name_short, weekday_name, weekday_short}}`), \
             not chrono — `ui/events.md` § Where logic lives. Offending sites:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// No painted text may hand-roll a local absolute date that shared Rust
    /// already owns.
    ///
    /// This is the sibling concern to the month/weekday guard above, on a
    /// different axis: those specifiers are banned because they bake *English*
    /// outside the catalog; these are banned because
    /// `behavior/value-formatting.md` § Absolute local timestamp display puts
    /// the `YYYY-MM-DD` and `YYYY-MM-DD HH:MM` renders — and their
    /// never-invent-a-time fallback — in `fauna_core::format`, and a private
    /// copy silently forks from it. It had already forked: all five tui copies
    /// returned the **empty string** out of chrono's range, where the shared
    /// contract returns the raw number, so a corrupt timestamp rendered exactly
    /// like an unset one. linux never had the bug — it single-sources through
    /// `i18n::local_date`, one line over the same shared fn.
    ///
    /// The guard walks the tree rather than listing the five, for the same
    /// reason its neighbour does: the copy worth catching is the sixth. Only
    /// the two shared-owned shapes are named — a locale-aware short form (glib
    /// `%b %-d` and friends) is a legitimately platform-side render and is not
    /// this test's business.
    ///
    /// Like its neighbour it assembles each needle from letters rather than
    /// writing the specifier literally, which is what lets it walk **its own
    /// file** with no exemption.
    #[test]
    fn no_painted_text_hand_rolls_a_shared_owned_local_date() {
        // Assembled, never written literally — see the doc comment.
        let y = format!("%{}", 'Y');
        let m = format!("%{}", 'm');
        let d = format!("%{}", 'd');
        let h = format!("%{}", 'H');
        let min = format!("%{}", 'M');
        let date = format!("{y}-{m}-{d}");
        let date_time = format!("{date} {h}:{min}");
        let shared_owned: &[(&str, &str)] = &[
            (date.as_str(), "fauna_core::format::format_unix_local_date"),
            (
                date_time.as_str(),
                "fauna_core::format::format_unix_local (or its `_ms` adapter)",
            ),
        ];

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let offenders = fauna_core::format::find_hand_rolled_format_calls(&src, shared_owned);
        assert!(
            offenders.is_empty(),
            "a local absolute date/datetime must come from shared Rust \
             (`fauna_core::format::{{format_unix_local, format_unix_local_date, \
             format_unix_local_date_ms}}`), not a private chrono chain — \
             `behavior/value-formatting.md` § Absolute local timestamp display. \
             The private copies also differ: they render EMPTY out of range \
             where the shared contract renders the raw number. Offending sites:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// A long-past timestamp falls back to the plain local date.
    #[test]
    fn a_distant_timestamp_renders_an_absolute_date() {
        // 2021-01-01T00:00:00Z in micros — far outside any relative bucket.
        let rendered = format_epoch_us(1_609_459_200_000_000);
        assert!(
            rendered.starts_with("202"),
            "expected a %Y-%m-%d date, got {rendered:?}"
        );
    }
}
