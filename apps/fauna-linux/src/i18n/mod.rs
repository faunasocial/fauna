//! Internationalized strings and localized display-value formatting for
//! fauna-linux.
//!
//! The string table (`strings`) is the **shared** `fauna-i18n` crate, re-exported
//! under this module's own path so every `crate::i18n::strings::…` call site in
//! linux resolves unchanged. It is auto-generated from `i18n/strings/en.yaml` —
//! run `just i18n-generate`, never edit it by hand. The free functions below
//! resolve the shared `fauna_core::format` (and `fauna_provisioning::progress`)
//! *decisions* — which computed the bucket / unit once in shared Rust and
//! returned a [`fauna_core::localized::LocalizedText`] — into display strings via
//! that table, so no English or threshold is hand-rolled per client (priority
//! #2/#3, `docs/goal/behavior/value-formatting.md`).
//!
//! **Why a re-export and not a generated file here** (2026-08-19, priority #2/#4):
//! the generator's `emit_rust` had two registered targets producing *byte-identical*
//! output — this app's `src/i18n/strings.rs` and `libs/fauna-i18n/src/strings.rs`
//! — so linux compiled its own 13.5k-line copy of a table the shared crate already
//! carried. tui had already taken the shared crate directly (its `Cargo.toml` says
//! so, naming linux's file as the same full `emit_rust` output); linux was the last
//! app holding a redundant Rust twin, and the twin is now deleted rather than
//! replicated. The re-export keeps the *call-site* idiom (`crate::i18n::strings`)
//! that ~13k linux references and the i18n-coverage lint's alias regex both
//! already speak.

pub use fauna_i18n::strings;

/// Resolve a bare i18n key to its string, falling back to the key itself if
/// unknown — for dynamic keys assembled at runtime (e.g. `provisioning.<provider>.*`)
/// that the generated `admin::*`/`onboarding::*` constants can't name statically.
pub fn resolve_key(key: &str) -> String {
    strings::lookup(key)
        .map(|s| s.to_string())
        .unwrap_or_else(|| key.to_string())
}

/// What a notification row says: the shared
/// [`fauna_client_notifications::notification_text`] decision (localized body,
/// English `summary`, or the default — `behavior/notifications.md`
/// § Localized body), its localized arm resolved through this app's table.
/// The row and its desktop toast both paint this, so the two never disagree.
pub fn notification_text(item: &fauna_client_notifications::notifications::NotifItem) -> String {
    resolve_notification_text(fauna_client_notifications::notification_text(item))
}

/// [`notification_text`] for a `fauna.notification` push — the desktop toast
/// raised off the push says what the row it announces says.
pub fn notification_push_text(push: &fauna_protocol::push_events::NotificationPayload) -> String {
    resolve_notification_text(fauna_client_notifications::notification_push_text(push))
}

/// The same decision for a `fauna.knock` push — the knock toast says what the
/// knock's row says (`fauna_client_notifications::knock_push_text`).
pub fn knock_push_text(push: &fauna_protocol::push_events::KnockPayload) -> String {
    resolve_notification_text(fauna_client_notifications::knock_push_text(push))
}

fn resolve_notification_text(text: fauna_client_notifications::NotificationText) -> String {
    use fauna_client_notifications::NotificationText;
    match text {
        NotificationText::Localized(text) => text.resolve(strings::lookup),
        NotificationText::Verbatim(text) => text,
    }
}

/// Resolve a shared [`fauna_core::format::byte_size`] decision (the 1024-unit +
/// rounding is chosen once in shared Rust) into a display string via the
/// generated linux string table. Clients must not hand-roll the bucketing —
/// see `docs/goal/behavior/value-formatting.md`.
pub fn byte_size(bytes: u64) -> String {
    fauna_core::format::byte_size(bytes).resolve(strings::lookup)
}

/// Resolve a shared [`fauna_core::format::duration_secs`] (coarse uptime)
/// decision into a display string.
pub fn duration_secs(secs: u64) -> String {
    fauna_core::format::duration_secs(secs).resolve(strings::lookup)
}

/// A tip amount on the shared sats/msats scale (`post-tip-total`, and each
/// `post-tip-item`'s amount) — `monetization.md` § Tips. The unit split
/// (sats above 1 sat, msats below) is a shared decision, exactly like the
/// byte scale above; this only resolves it against the linux table.
///
/// The three tip resolvers here excise with the plane they format for
/// (`dynamic-features.md` § Platform-family surface excision), matching tui's
/// `format.rs` twins — their only callers are the gated tip render.
#[cfg(feature = "payments")]
pub fn tip_amount(msats: i64) -> String {
    fauna_core::format::tip_amount(msats).resolve(strings::lookup)
}

/// A tip count with its singular (`post-tip-count`) — shared so no client
/// ships "1 tips".
#[cfg(feature = "payments")]
pub fn tip_count(count: i64) -> String {
    fauna_core::format::tip_count(count).resolve(strings::lookup)
}

/// The `post-tip-list` tail, "and N more", for a bounded attribution window.
/// `n` comes from the nest's own `has_more` + totals, never from comparing
/// the rendered row count against a cap this client hard-codes.
#[cfg(feature = "payments")]
pub fn tip_more(n: i64) -> String {
    fauna_core::format::tip_more(n).resolve(strings::lookup)
}

/// How many events fall on a calendar day, with its singular (the month-grid
/// `events-day-cell` accessibility tooltip) — shared so no client ships
/// "1 events". The caller renders the count clause only when `count > 0`.
pub fn event_count(count: i64) -> String {
    fauna_core::format::event_count(count).resolve(strings::lookup)
}

/// Resolve a shared [`fauna_core::format::grace_countdown`] decision (a
/// countdown to a future deadline, both epoch-ms) into a display string.
/// `None` once the deadline has passed — the caller renders its own
/// already-localized "elapsed" label in that case.
pub fn grace_countdown(deadline_ms: i64, now_ms: i64) -> Option<String> {
    fauna_core::format::grace_countdown(deadline_ms, now_ms).map(|lt| lt.resolve(strings::lookup))
}

/// Resolve the shared [`fauna_provisioning::progress::elapsed_display`]
/// `provisioning-elapsed` ticker decision into a display string. `None` until
/// the run has started (hide the row); freezes at `finished_at_ms`. The
/// subtraction + i18n glue is single-sourced in shared Rust — see
/// `docs/goal/behavior/value-formatting.md`.
pub fn provisioning_elapsed(
    started_at_ms: Option<u64>,
    finished_at_ms: Option<u64>,
    now_ms: u64,
) -> Option<String> {
    fauna_provisioning::progress::elapsed_display(started_at_ms, finished_at_ms, now_ms)
        .map(|lt| lt.resolve(strings::lookup))
}

/// Resolve the shared [`fauna_provisioning::progress::step_label`] provisioning
/// step-name decision into a display string via the generated linux table. The
/// step→key mapping is single-sourced in shared Rust (priority #2/#3); this is
/// the thin resolve, mirroring [`provisioning_elapsed`].
pub fn provisioning_step_label(kind: fauna_provisioning::progress::ProvisionStep) -> String {
    fauna_provisioning::progress::step_label(kind).resolve(strings::lookup)
}

/// Resolve the shared [`fauna_provisioning::progress::substep_label`]
/// provisioning sub-step decision into a display string. `cause` (the step's
/// `last_error`) fills the `{cause}` arg of the `status_retrying` substep; every
/// other substep ignores it. Single-sourced in shared Rust — see
/// `docs/goal/behavior/value-formatting.md`.
pub fn provisioning_substep_label(
    key: fauna_provisioning::progress::SubstepKey,
    cause: Option<String>,
) -> String {
    fauna_provisioning::progress::substep_label(key, cause).resolve(strings::lookup)
}

/// Resolve the shared [`fauna_core::format::connection_state_label`] decision
/// (the `ConnectionState` → i18n-key mapping, single-sourced so linux/web/
/// native can't disagree) into a display string for the `connection-status`
/// indicator. `state` is the lowercase wire word (`"connected"` /
/// `"connecting"` / `"disconnected"` / `"unreachable"`) — see
/// [`fauna_ws_substrate::supervisor::ConnectionState`] callers.
pub fn connection_state_label(state: &str) -> String {
    fauna_core::format::connection_state_label(state).resolve(strings::lookup)
}

/// Resolve the shared [`fauna_client_bridges::nostr_key_source_label`] Nostr
/// signing-mode decision (the stored `generated`/`imported`/`remote`/`nip07`
/// value) into a display string for the Signing Mode row, so the raw enum is
/// never shown (priority #2/#4, `docs/goal/ui/nostr.md` § Account linking). An
/// unknown/placeholder mode (e.g. the `—` shown when not linked) renders
/// verbatim.
pub fn nostr_signing_mode_label(mode: &str) -> String {
    fauna_client_bridges::nostr_key_source_label(mode).resolve(strings::lookup)
}

/// Resolve the shared [`fauna_client_bridges::nostr_link_mode_label`] Nostr
/// link-*request*-mode decision (the `generate`/`import`/`remote` value the
/// `nostr-link-mode` picker sends) into a display string, so the picker's
/// combo-row entries aren't a per-app match (priority #2/#4,
/// `docs/goal/ui/nostr.md` § Account linking).
pub fn nostr_link_mode_label(mode: &str) -> String {
    fauna_client_bridges::nostr_link_mode_label(mode).resolve(strings::lookup)
}

/// Resolve the shared [`fauna_core::format::media_sort_label`] `media-sort-select`
/// value → key decision into a display string, so the dropdown's label render
/// isn't a per-app match (priority #2, `docs/goal/ui/media.md` § Layout & flow).
pub fn media_sort_label(value: &str) -> String {
    fauna_core::format::media_sort_label(value).resolve(strings::lookup)
}

/// Resolve the shared [`fauna_protocol::email::filter_action_label`]
/// `filter-action` badge decision into a display string, so the label render
/// isn't a per-app match (priority #2/#4).
pub fn email_filter_action_label(action: &fauna_protocol::email::EmailFilterAction) -> String {
    fauna_protocol::email::filter_action_label(action).resolve(strings::lookup)
}

/// Resolve the shared [`fauna_core::format::relative_time`] bucket decision into
/// a display string. The four relative buckets ("just now" / "{count}m ago" /
/// "{count}h ago" / "{count}d ago") render their i18n key through the generated
/// table; a timestamp `≥ 7 d` old (the shared
/// [`fauna_core::format::RelativeTimestamp::Absolute`] bucket) renders a real
/// local date. Both args are epoch **milliseconds**. Clients must not hand-roll
/// the thresholds or the English — see
/// `docs/goal/behavior/value-formatting.md` § Relative time.
///
/// The bucket-or-date resolve itself is
/// [`fauna_core::format::relative_time_text`] now: it was identical here and in
/// tui, so only the argument order (this door takes `then, now`) and the lookup
/// stay linux's.
pub fn relative_time(then_ms: i64, now_ms: i64) -> String {
    fauna_core::format::relative_time_text(now_ms, then_ms, strings::lookup)
}

/// Resolve the shared `backup-audit-alert` banner text
/// ([`fauna_core::format::backup_audit_alert_label`]) for one failing
/// destination. Complete as-is — the destination name and the reason's numbers
/// are already substituted shared-side.
pub fn backup_audit_alert(
    reason: fauna_core::format::BackupAuditAlertReason,
    destination_label: &str,
) -> String {
    fauna_core::format::backup_audit_alert_label(reason, destination_label).resolve(strings::lookup)
}

/// Resolve the `backup-destination-kind-badge` text
/// ([`fauna_core::format::backup_destination_kind_label`]) — one level, so a
/// plain resolve; the unknown-kind arm carries its `{kind}` as an ordinary arg,
/// which is how a kind a newer client wrote renders **as itself** rather than
/// masquerading as a nest.
pub fn backup_destination_kind(kind: &str) -> String {
    fauna_core::format::backup_destination_kind_label(kind).resolve(strings::lookup)
}

/// The `personalization-trained-factor-publish-kind-select` option text, via
/// the shared `fauna_core::format::publish_kind_label`. Paint-only: the select
/// round-trips the wire discriminator, so this never becomes a driver
/// contract (the `backup_destination_kind` twin).
pub fn publish_kind(kind: &str) -> String {
    fauna_core::format::publish_kind_label(kind).resolve(strings::lookup)
}

/// The Model review row's class-direction word, via the shared
/// `fauna_core::format::ngram_direction_label` — the same face the
/// subscriber's `labeler-inspect-model-entry-direction` reads, so the
/// publisher's review and what a subscriber sees cannot disagree.
pub fn ngram_direction(more: u32, less: u32) -> String {
    fauna_core::format::ngram_direction_label(more, less).resolve(strings::lookup)
}

/// The Model review row's class-blind distinct-post count, via the shared
/// `fauna_core::format::ngram_doc_count_label`.
pub fn ngram_doc_count(more: u32, less: u32) -> String {
    fauna_core::format::ngram_doc_count_label(more, less).resolve(strings::lookup)
}

/// The `labeler-catalog-item-kind` badge's override text, via the shared
/// `fauna_core::format::text_model_needs_newer_app` — `None` means paint the
/// kind verbatim (every ordinary row); `Some` is the one non-passthrough
/// field, resolved here so the badge and the compose seam's inert branch stay
/// unable to disagree (both read `scoring::text_model_version_supported`).
pub fn text_model_needs_newer_app(artifact_kind: &str, artifact_version: u64) -> Option<String> {
    fauna_core::format::text_model_needs_newer_app(artifact_kind, artifact_version)
        .map(|t| t.resolve(strings::lookup))
}

/// The `backup-destination-usage` row text
/// ([`fauna_core::format::backup_usage_text`]) — held bytes against the
/// user-set cap, client-device rows only.
///
/// Two-level like [`fauna_client_backup::row_text::destination_last_upload_text`],
/// but with **two** inner texts rather than one: both byte sizes are
/// themselves `LocalizedText` (unit key + value),
/// so each resolves before substitution — shared-side, since tui composed them
/// exactly the same way. The cap-reached decision is *not* made here either —
/// the shared fn reads `cap_state` and never infers it from `held >= cap`,
/// because a pass that stopped at its cap ends *below* it and inference would
/// render a stalled backup as healthy-with-room.
pub fn backup_usage(
    held_bytes: Option<u64>,
    capacity_cap_bytes: Option<u64>,
    cap_state: Option<&str>,
) -> String {
    fauna_core::format::backup_usage_text(
        held_bytes,
        capacity_cap_bytes,
        cap_state,
        strings::lookup,
    )
}

/// Format an epoch-**millisecond** timestamp as a local absolute date
/// (`YYYY-MM-DD`) — now a bare alias for the shared
/// [`fauna_core::format::format_unix_local_date_ms`] (`%Y-%m-%d` has no
/// locale-varying component, so it IS a shared-Rust target — unlike
/// [`local_short_date`]'s `%b`, which needs glib's locale month names and
/// stays platform shell). Kept as a name because the feed/search relative-time
/// `≥ 7 d` fallback ([`relative_time`]) and the mail-settings "last hit" /
/// "created" rows all read better through it. Falls back to the raw seconds on
/// the (unreachable) conversion error — never invents a time.
///
/// The ms→secs step moved *into* shared Rust when tui's five hand-rolled copies
/// were lifted: it must floor rather than truncate toward zero, which the `ms /
/// 1000` that used to sit here got wrong for a pre-1970 sub-second instant.
pub fn local_date(ms: i64) -> String {
    fauna_core::format::format_unix_local_date_ms(ms)
}

/// Format an epoch-**millisecond** timestamp as a short, locale-aware month-day
/// (`Jun 12`) via GTK's `glib::DateTime` — the conversation-list "older" form.
/// Falls back to [`local_date`] (`YYYY-MM-DD`) on the (unreachable) format error.
pub fn local_short_date(ms: i64) -> String {
    match gtk::glib::DateTime::from_unix_local(ms / 1000) {
        Ok(dt) => dt
            .format("%b %-d")
            .map(|s| s.to_string())
            .unwrap_or_else(|_| local_date(ms)),
        Err(_) => local_date(ms),
    }
}

/// Resolve the shared [`fauna_core::format::conversation_timestamp`] contextual
/// bucket into a conversation-list display string, in the user's local timezone
/// ([`crate::logs_view::local_offset_secs`]): today → a local clock (`14:30`);
/// Yesterday / a weekday → the i18n key via the generated table; `≥ 7 d` old →
/// a real local short date via [`local_short_date`]. Both args are epoch
/// **milliseconds**. Clients must not hand-roll the buckets or the English —
/// see `docs/goal/behavior/value-formatting.md` § Conversation timestamp.
pub fn conversation_timestamp(then_ms: i64, now_ms: i64) -> String {
    let off = crate::logs_view::local_offset_secs();
    let d = fauna_core::format::conversation_timestamp_display(now_ms, then_ms, off);
    if let Some(clock) = d.clock {
        clock
    } else if let Some(lt) = d.localized {
        lt.resolve(strings::lookup)
    } else if let Some(epoch_ms) = d.absolute_epoch_ms {
        local_short_date(epoch_ms)
    } else {
        String::new()
    }
}

/// Resolve the shared custody receipt-status decision
/// (`custody_receipt_status_text`) — the A7 three-state honesty rule, where
/// fresh / stale / no-receipt-yet are three different strings that never
/// collapse (`docs/goal/ui/devices.md` § Custody facet).
///
/// The resolve (label + `{when}` substitution) moved into
/// `fauna-client-capabilities` — it was
/// byte-identical to tui's twin, the same shape the backup family's `*_text`
/// doors already share. This is now a bare lookup-table forward.
pub fn custody_receipt_status(
    state: fauna_client_capabilities::view_model::ReceiptState,
    attested_at_micros: Option<u64>,
) -> String {
    fauna_client_capabilities::view_model::custody_receipt_status_text(
        state,
        attested_at_micros,
        strings::lookup,
    )
}

/// Resolve the shared custody held-bytes decision
/// (`custody_held_bytes_text`) into the `custody-holder-held-bytes` line.
///
/// The resolve moved into `fauna-client-capabilities` alongside its receipt-
/// status sibling — it was already byte-identical to
/// tui's twin once both apps shared one string table, contrary
/// to the divergence the row was originally filed against.
pub fn custody_held_bytes(
    receipt: Option<&fauna_client_capabilities::view_model::CustodyReceiptView>,
) -> String {
    fauna_client_capabilities::view_model::custody_held_bytes_text(receipt, strings::lookup)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed "now" (epoch ms) so the buckets are deterministic.
    const NOW: i64 = 1_700_000_000_000;

    fn ago(ms: i64) -> String {
        relative_time(NOW - ms, NOW)
    }

    /// The knock toast paints the knock row's own sentence for a known key
    /// (`behavior/notifications.md` § Localized body), and the toast's own
    /// sentence from a nest that sends no body.
    #[test]
    fn a_knock_toast_says_what_its_row_says() {
        let sender = "a1b2c3d4".repeat(8);
        let mut push = fauna_protocol::push_events::KnockPayload {
            sender_id: sender.clone(),
            summary: "hi".into(),
            body: Some(
                fauna_protocol::LocalizedText::new("notifications.row_knock")
                    .with_arg("sender", &sender[..8])
                    .with_arg("message", "hi"),
            ),
            ..Default::default()
        };
        assert_eq!(knock_push_text(&push), "a1b2c3d4 wants to connect: hi");
        push.body = None;
        assert_eq!(knock_push_text(&push), "a1b2c3d4 wants to connect");
    }

    #[test]
    fn relative_time_resolves_shared_buckets_through_the_i18n_table() {
        assert_eq!(ago(30 * 1_000), "just now");
        assert_eq!(ago(5 * 60 * 1_000), "5m ago");
        assert_eq!(ago(3 * 60 * 60 * 1_000), "3h ago");
        assert_eq!(ago(2 * 24 * 60 * 60 * 1_000), "2d ago");
        // A future timestamp clamps to "just now" (shared bucket clamps `diff < 0`).
        assert_eq!(relative_time(NOW + 60_000, NOW), "just now");
    }

    #[test]
    fn relative_time_renders_an_absolute_date_past_a_week() {
        // `≥ 7 d` old → the shared `Absolute` bucket → a real local date,
        // never an "…ago" string.
        let s = ago(30 * 24 * 60 * 60 * 1_000);
        assert!(!s.is_empty());
        assert!(!s.ends_with("ago"), "expected an absolute date, got {s:?}");
    }

    /// A hand-rolled chrono `.format` chain rendering the `YYYY-MM-DD` or
    /// `YYYY-MM-DD HH:MM` shape re-implements a shared-owned render this app
    /// must not duplicate —
    /// `docs/goal/behavior/value-formatting.md` § Absolute local timestamp
    /// display + "The date-only sibling, and its milliseconds door". linux
    /// already single-sources both forms through [`local_date`] (a bare alias
    /// for `fauna_core::format::format_unix_local_date_ms`); this is a decay
    /// guard, not a fix — sweep found windows had re-grown three hand-rolls
    /// of this exact shape within a month of being declared clean, with no
    /// structural pin to catch it. Sibling of tui's
    /// `no_painted_text_hand_rolls_a_shared_owned_local_date`
    /// (`apps/fauna-tui/src/format.rs`).
    ///
    /// Only the two shared-owned shapes are named — [`local_short_date`]'s
    /// glib `%b %-d` locale-aware short form is legitimately platform-side and
    /// is not this test's business.
    ///
    /// Needles are assembled from letters, never written literally, so the
    /// guard can walk its own file with no exemption.
    #[test]
    fn no_painted_text_hand_rolls_a_shared_owned_local_date() {
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
}
