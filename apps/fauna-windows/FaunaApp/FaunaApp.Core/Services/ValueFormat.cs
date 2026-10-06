using System.Linq;
using uniffi.fauna_ffi;
// The provisioning step enums live in the fauna-provisioning bindings; targeted
// aliases (not a broad import) so they don't collide with same-named fauna_ffi types.
using StepStatus = uniffi.fauna_provisioning.StepStatus;
using ProvisionStep = uniffi.fauna_provisioning.ProvisionStep;
using SubstepKey = uniffi.fauna_provisioning.SubstepKey;

namespace FaunaApp.Core.Services;

/// <summary>
/// Human-readable display formatting (byte sizes, durations/uptime) for the
/// windows app. The <em>decision</em> — which 1024-unit, which duration
/// units, the rounding — lives once in shared Rust (<c>fauna_core::format</c>,
/// per <c>docs/goal/behavior/value-formatting.md</c>) and is returned as an
/// i18n key + args; this just resolves that <c>LocalizedText</c> through the
/// windows <see cref="Strings"/> pipeline. Clients MUST NOT hand-roll the
/// thresholds or the English unit strings (priority #2; #4 — one shared shape
/// across all 7 apps, mirroring web's <c>value-format.ts</c> and linux's
/// <c>i18n::byte_size</c>/<c>duration_secs</c>).
/// </summary>
public static class ValueFormat
{
    /// <summary>Largest 1024-unit ≥ 1, ≤ one decimal (trailing <c>.0</c>
    /// dropped): "512 B", "1.5 KB", "3 GB", "1 TB".</summary>
    public static string ByteSize(ulong bytes) => Strings.Resolve(FaunaFfiMethods.ByteSize(bytes));

    /// <summary>Coarse d/h/m uptime/duration — the largest non-zero unit down to
    /// minutes, always the full chain below it, seconds dropped: "0m", "1h 0m",
    /// "5d 2h 30m". The multi-arg <c>{days,hours,mins}</c> key resolves by name
    /// (order-robust), so it no longer trips the former positional-resolver bug.</summary>
    public static string DurationSecs(ulong secs) => Strings.Resolve(FaunaFfiMethods.DurationSecs(secs));

    /// <summary>Relative timestamp for recent items ("just now", "5m ago", …,
    /// "6d ago"); <c>≥ 7 d</c> old renders an absolute date with the platform's
    /// native, locale-aware formatter. <paramref name="nowMs"/>/<paramref
    /// name="thenMs"/> are epoch <b>milliseconds</b> (UTC) — convert micros-based
    /// sources (feed/notifications <c>created_at</c>) with <c>÷1000</c> at the call
    /// site. The bucket decision lives in shared Rust (<c>relative_time_display</c>);
    /// a <c>null</c> localized result is the signal to format the absolute
    /// epoch.</summary>
    public static string RelativeTime(long nowMs, long thenMs) =>
        ResolveRelativeTimeDisplay(FaunaFfiMethods.RelativeTimeDisplay(nowMs, thenMs), thenMs);

    /// <summary>Resolve an already-computed <c>RelativeTimeDisplay</c> — the same
    /// null-localized → absolute-date fallback <see cref="RelativeTime"/> uses, for
    /// callers holding one from elsewhere (e.g. <c>BackupLastUploadDisplay.when</c>).</summary>
    internal static string ResolveRelativeTimeDisplay(uniffi.fauna_core.RelativeTimeDisplay display, long fallbackMs)
    {
        if (display.@localized is not null) return Strings.Resolve(display.@localized);
        var absMs = display.@absoluteEpochMs ?? fallbackMs;
        return DateTimeOffset.FromUnixTimeMilliseconds(absMs).LocalDateTime.ToString("d");
    }

    /// <summary>Contextual last-activity timestamp for a conversation/DM-list row
    /// (<c>conversations.md</c> § Layout — list-row timestamp): the local 24 h
    /// wall-clock (<c>"14:30"</c>) for today, "Yesterday" / a weekday abbreviation
    /// for the prior calendar week, else an absolute, native locale-aware date
    /// (<c>≥ 7</c> local days old). The calendar bucketing lives once in shared
    /// Rust (<c>conversation_timestamp_display</c>, per
    /// <c>docs/goal/behavior/value-formatting.md</c> § Conversation timestamp); the
    /// client passes its current UTC offset because shared Rust is WASM-safe and
    /// can't read the local zone. Clients MUST NOT hand-roll the buckets (priority
    /// #1/#2/#4 — one shape across all 7 apps). <paramref name="nowMs"/> /
    /// <paramref name="thenMs"/> are epoch <b>milliseconds</b> (UTC);
    /// <paramref name="utcOffsetSeconds"/> is the caller's local UTC offset in
    /// seconds. Exactly one of the returned <c>clock</c> / <c>localized</c> /
    /// <c>absoluteEpochMs</c> fields is set — the same null-localized → absolute
    /// path as <see cref="RelativeTime"/>.</summary>
    public static string ConversationTimestamp(long nowMs, long thenMs, int utcOffsetSeconds)
    {
        var display = FaunaFfiMethods.ConversationTimestampDisplay(nowMs, thenMs, utcOffsetSeconds);
        if (display.@clock is not null) return display.@clock;
        if (display.@localized is not null) return Strings.Resolve(display.@localized);
        var absMs = display.@absoluteEpochMs ?? thenMs;
        return DateTimeOffset.FromUnixTimeMilliseconds(absMs).LocalDateTime.ToString("d");
    }

    /// <summary>Nest-provisioning elapsed-time ticker
    /// (<c>onboarding.nest_provisioning.elapsed_template</c> + <c>{seconds}</c>):
    /// whole seconds since <paramref name="startedAtMs"/>, frozen at
    /// <paramref name="finishedAtMs"/> once the run terminates, else ticking
    /// against <paramref name="nowMs"/>. The ms→secs floor, the freeze, and the
    /// backwards-clock (<c>saturating</c>) guard live once in shared Rust
    /// (<c>fauna_provisioning::progress::elapsed_display</c>); a <c>null</c> result
    /// (no <c>started_at_ms</c> yet) renders as the empty string so the elapsed row
    /// stays hidden. All epoch values are <b>milliseconds</b>.</summary>
    public static string ProvisioningElapsed(ulong? startedAtMs, ulong? finishedAtMs, ulong nowMs)
    {
        var lt = FaunaFfiMethods.ProvisioningElapsed(startedAtMs, finishedAtMs, nowMs);
        return lt is null ? string.Empty : Strings.Resolve(lt);
    }

    /// <summary>Coarse d/h countdown to a future deadline ("2d 3h", "5h") — the
    /// mail primary-domain-rename <c>admin-dns-rename</c> grace-window banner's
    /// client-rendered ticker, computed once per render like the other apps'
    /// <c>grace_remaining</c>/<c>graceRemaining</c> (a snapshot, not a ticking
    /// timer). <c>null</c> once <paramref name="nowMs"/> reaches <paramref
    /// name="deadlineMs"/> — the wording for that elapsed state is per-surface,
    /// so the caller renders its own already-localized fallback
    /// (<c>docs/goal/behavior/value-formatting.md</c> § Grace countdown).</summary>
    public static string? GraceCountdown(long deadlineMs, long nowMs)
    {
        var lt = FaunaFfiMethods.GraceCountdown(deadlineMs, nowMs);
        return lt is null ? null : Strings.Resolve(lt);
    }

    /// <summary>Canonical status-column glyph for a provisioning step
    /// (<c>provisioning-step-checkbox</c>): <c>○</c>/<c>…</c>/<c>—</c>/<c>✓</c>/<c>✗</c>.
    /// The symbol set lives once in shared Rust
    /// (<c>fauna_provisioning::progress::status_glyph</c>) so all seven apps render
    /// the same glyph — a plain string (locale-invariant, no <c>LocalizedText</c>).
    /// Replaces the windows-local <c>StatusGlyph</c> (which used the web <c>⟳/−/✕</c>
    /// set; the shared set is linux/apple's <c>…/—/✗</c>).</summary>
    internal static string ProvisioningStatusGlyph(StepStatus status)
        => FaunaFfiMethods.ProvisioningStatusGlyph(status);

    /// <summary>The user-visible step name (<c>provisioning-step-label</c>). The
    /// canonical <c>onboarding.provision.step.*</c> i18n key lives in shared Rust
    /// (<c>fauna_provisioning::progress::step_label</c>); this resolves the returned
    /// <c>LocalizedText</c> through the windows <see cref="Strings"/> pipeline.</summary>
    internal static string ProvisioningStepLabel(ProvisionStep kind)
        => Strings.Resolve(FaunaFfiMethods.ProvisioningStepLabel(kind));

    /// <summary>The sub-step text (<c>provisioning-substep</c>). The
    /// <c>onboarding.provision.substep.*</c> key lives in shared Rust
    /// (<c>fauna_provisioning::progress::substep_label</c>); for <c>StatusRetrying</c>
    /// the <c>{cause}</c> placeholder is filled from <paramref name="cause"/> (the
    /// step's <c>last_error</c>) so the user sees the real cause, not a literal
    /// <c>{cause}</c>. Resolved through the windows <see cref="Strings"/> pipeline.</summary>
    internal static string ProvisioningSubstepLabel(SubstepKey key, string? cause)
        => Strings.Resolve(FaunaFfiMethods.ProvisioningSubstepLabel(key, cause));

    /// <summary>Abbreviated weekday name for a calendar grid header (<c>events.md</c>
    /// § Layout &amp; flow, month/week views), resolved through the generated i18n
    /// catalog (<c>time/weekday_mon</c>…<c>_sun</c> — the same keys the relative-time
    /// "N days ago" bucket already uses) rather than the OS's native locale, so it
    /// tracks the app's own translation, not whatever language the OS happens to be
    /// in (<c>docs/goal/ui/events.md:139</c>: localized weekday names are per-app
    /// i18n, not <c>fauna_core</c>'s or the OS's). Mirrors linux's identical fix in
    /// <c>time_utils.rs::weekday_short</c> (replaces the hardcoded <c>{"Sun","Mon",...}</c>
    /// array in <c>EventsPage.BuildMonthGrid</c> and <c>DayOfWeek.ToString()</c> — always
    /// English — in <c>BuildWeekGrid</c>).</summary>
    public static string WeekdayAbbreviation(DayOfWeek day) => day switch
    {
        DayOfWeek.Sunday => Strings.Get("time/weekday_sun"),
        DayOfWeek.Monday => Strings.Get("time/weekday_mon"),
        DayOfWeek.Tuesday => Strings.Get("time/weekday_tue"),
        DayOfWeek.Wednesday => Strings.Get("time/weekday_wed"),
        DayOfWeek.Thursday => Strings.Get("time/weekday_thu"),
        DayOfWeek.Friday => Strings.Get("time/weekday_fri"),
        DayOfWeek.Saturday => Strings.Get("time/weekday_sat"),
        _ => throw new ArgumentOutOfRangeException(nameof(day)),
    };

    /// <summary>Raw-value options for the trained-factor publish sheet's kind
    /// select (<c>personalization-trained-factor-publish-kind-select</c>,
    /// <c>topic-factors.md</c> § Publishing a trained factor, v2): List
    /// (default — the weaker disclosure) | Model. <c>Value</c> is the raw
    /// <c>artifact_kind</c> wire discriminator a driver round-trips; <c>Label</c>
    /// is already resolved for display. <c>fauna_core::format::publish_kind_options</c>
    /// owns the ordering and wording for all 7 apps — clients must not hand-roll
    /// the option list.</summary>
    public static (string Value, string Label)[] PublishKindOptions() =>
        FaunaFfiMethods.PublishKindOptions().Select(o => (o.@value, Strings.Resolve(o.@label))).ToArray();

    /// <summary>The Model-kind n-gram review row's class-direction column
    /// ("More like this" / "Less like this" / "Both") — the SAME shared face
    /// <c>labeler-inspect-model-entry-direction</c> reads on the subscriber
    /// side, so a publisher's review and a subscriber's inspect cannot disagree
    /// about what the counts mean (<c>fauna_core::format::ngram_direction_label</c>).</summary>
    public static string NgramDirectionLabel(uint more, uint less) =>
        Strings.Resolve(FaunaFfiMethods.NgramDirectionLabel(more, less));

    /// <summary>The Model-kind n-gram review row's distinct-document count —
    /// the CLASS-BLIND sum (more + less), the same quantity the shared 3-post
    /// prune floor bounds (<c>fauna_core::format::ngram_doc_count_label</c>).</summary>
    public static string NgramDocCountLabel(uint more, uint less) =>
        Strings.Resolve(FaunaFfiMethods.NgramDocCountLabel(more, less));

    /// <summary>The catalog kind-badge override (<c>labeler-catalog-item-kind</c>)
    /// for a <c>text-model</c> artifact whose tokenizer contract this build does
    /// not implement — the compose seam leaves that factor inert, and this badge
    /// is where the user learns why (<c>content-moderation-and-ranking.md</c> §
    /// Tier-3 artifact kinds). The predicate and wording are both shared
    /// (<c>fauna_core::format::text_model_needs_newer_app</c>, over
    /// <c>scoring::text_model_version_supported</c> — the same function the
    /// compose seam's inert branch reads), so a badge that disagreed with the
    /// scorer is not expressible here. <c>null</c> = paint the kind verbatim,
    /// which is every ordinary row.</summary>
    public static string? TextModelNeedsNewerApp(string artifactKind, ulong artifactVersion)
    {
        var lt = FaunaFfiMethods.TextModelNeedsNewerApp(artifactKind, artifactVersion);
        return lt is null ? null : Strings.Resolve(lt);
    }
}
