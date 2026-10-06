using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.ViewModels;

public partial class EventDetailViewModel : ViewModelBase
{
    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string _summary = string.Empty;
    [ObservableProperty] private string _timeRange = string.Empty;
    [ObservableProperty] private string? _description;
    [ObservableProperty] private string? _location;
    [ObservableProperty] private string? _rsvpStatus;
    [ObservableProperty] private string? _eventId;
    [ObservableProperty] private string _dtStart = string.Empty;
    [ObservableProperty] private string _dtEnd = string.Empty;

    /// <summary>True iff the calling actor organizes this event — gates the
    /// author-only affordances (delete / invite); the RSVP buttons show when this
    /// is <c>false</c> (an event the actor was invited to). Mirrors the shared
    /// <c>FfiCalEvent.organized_by_me</c> projection.</summary>
    [ObservableProperty] private bool _organizedByMe;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ReminderCurrentLabel), nameof(HasReminder))]
    private string? _reminderOffset;

    public ObservableCollection<AttendeeInfo> Attendees { get; } = new();

    /// <summary>True once the caller has a reminder set on this event — drives the
    /// select+Set ⇄ current-label+Remove flip on the detail surface.</summary>
    public bool HasReminder => ReminderOffset is not null;

    /// <summary>Human label for the current reminder offset (e.g. "1 hour before"),
    /// or empty when none is set. The offset→label map is shared Rust
    /// (<c>fauna_core::ical::reminder_label</c>, via the value-format FFI wrapper) —
    /// the three presets carry their <c>events.reminder.*</c> i18n key, a non-preset
    /// offset renders verbatim — resolved through the windows i18n pipeline. Single
    /// source of truth across clients (priority #1/#2/#4); previously a per-app
    /// hard-coded map.</summary>
    public string ReminderCurrentLabel =>
        ReminderOffset is { } o
            ? Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.ReminderLabel(o))
            : string.Empty;

    // The whole event detail surface (get / rsvp / delete / invite / reminder)
    // rides the encrypted CalDAV store via the shared FfiCaldavClient seam — the
    // single get_event carries the attendees + reminder inline, so there is no
    // separate attendees / reminder fetch (events.md § Encrypted store).
    private readonly INestRpcClient _rpc;

    internal EventDetailViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
    }

    [RelayCommand]
    private async Task LoadAsync(string eventId)
    {
        EventId = eventId;
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            // One get_event carries the whole detail — summary/time, the inline
            // attendee roster, the caller's reminder, and the organized-by-me
            // author flag — so no separate attendee / reminder fetch.
            var ev = await _rpc.CaldavGetEventAsync(eventId);
            if (ev is null)
            {
                ShowError(new InvalidOperationException("Event not found"));
                return;
            }
            Summary = ev.summary;
            Description = ev.description;
            Location = ev.location;
            DtStart = ev.dtstart;
            DtEnd = ev.dtend ?? string.Empty;
            TimeRange = $"{DtStart} – {DtEnd}";
            ReminderOffset = ev.reminder;
            OrganizedByMe = ev.organizedByMe;

            Attendees.Clear();
            foreach (var a in ev.attendees)
                Attendees.Add(new AttendeeInfo(a.email, a.name, a.rsvp));
        }
        catch (Exception ex) { ShowError(ex); }
        finally { IsLoading = false; }
    }

    [RelayCommand]
    private async Task RsvpAsync(string status)
    {
        if (EventId is null) return;
        ErrorMessage = null;
        try
        {
            // The detail page passes the CalDAV response vocabulary directly
            // (going / interested / declined) — no bare-verb form to normalize.
            var response = Enum.Parse<uniffi.fauna_core.RsvpResponse>(status, ignoreCase: true);
            await _rpc.CaldavRsvpEventAsync(EventId, response);
            RsvpStatus = status;
            await LoadAsync(EventId);
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    private async Task DeleteAsync()
    {
        if (EventId is null) return;
        ErrorMessage = null;
        try
        {
            await _rpc.CaldavDeleteEventAsync(EventId);
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    private async Task InviteAsync(string email)
    {
        if (EventId is null || string.IsNullOrWhiteSpace(email)) return;
        ErrorMessage = null;
        try
        {
            await _rpc.CaldavInviteAttendeeAsync(EventId, email.Trim());
            await LoadAsync(EventId);
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary><c>set_reminder</c> — set the caller's reminder to
    /// <paramref name="offset"/> (an ICS duration like <c>PT1H</c>); the detail
    /// surface flips to the current-reminder view once <c>ReminderOffset</c> lands.</summary>
    [RelayCommand]
    private async Task SetReminderAsync(string offset)
    {
        if (EventId is null || string.IsNullOrEmpty(offset)) return;
        ErrorMessage = null;
        try
        {
            await _rpc.CaldavSetReminderAsync(EventId, offset);
            ReminderOffset = offset;
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary><c>set_reminder("")</c> — clear the caller's reminder; the detail
    /// surface flips back to the preset select.</summary>
    [RelayCommand]
    private async Task RemoveReminderAsync()
    {
        if (EventId is null) return;
        ErrorMessage = null;
        try
        {
            await _rpc.CaldavSetReminderAsync(EventId, "");
            ReminderOffset = null;
        }
        catch (Exception ex) { ShowError(ex); }
    }
}

/// <summary>
/// View model for one attendee row in the EventDetail attendee list.
/// Mirrors the canonical row shape from android/web/apple (attendee-rows decision,
/// events.md § Attendee list presentation, 2026-06-23): monogram circle + display name
/// + email beneath (omitted when name == email) + colored RSVP status.
/// </summary>
public record AttendeeInfo(string Email, string Name, string Rsvp)
{
    // Shared attendee-row text projection (events.md § Attendee list presentation) — one source
    // of truth in fauna_core::ical::attendee_display, consumed over UniFFI. NOT a backing field:
    // a record's synthesized equality includes instance FIELDS, so a cached field would make two
    // otherwise-equal AttendeeInfos compare unequal. A computed get-only property has no field and
    // is excluded from equality; the projection is a trivial pure string fn, so recomputing is cheap.
    private uniffi.fauna_core.AttendeeDisplay Display =>
        uniffi.fauna_ffi.FaunaFfiMethods.AttendeeDisplay(Name, Email);

    /// <summary>Single uppercase character for the monogram circle; the email's initial for an
    /// email-only attendee, '?' only when there is no displayable character (events.md § Attendee
    /// list presentation).</summary>
    public string Monogram => Display.monogram;

    /// <summary>Primary display string: the CN when it is a real, distinct name, else the email.</summary>
    public string DisplayName => Display.displayName;

    /// <summary>Secondary line below DisplayName: the email when the name is a real distinct CN, else
    /// null (avoids showing the email twice).</summary>
    public string? EmailLine => Display.secondaryEmail;

    /// <summary>
    /// Localized RSVP status label (e.g. "Going") off the shared
    /// <c>fauna_core::ical::rsvp_status_label</c> via the value-format FFI wrapper —
    /// the single source of truth across clients (events.md § Attendee list
    /// presentation; priority #1/#2/#4). Replaces the prior raw-lowercase
    /// <c>{x:Bind Rsvp}</c> render (windows was the lone client that drifted on the
    /// label, showing the unformatted status). Computed get-only, not a backing
    /// field, for the same record-equality reason as the other AttendeeInfo
    /// projections. The status <b>color</b> stays the per-app <see cref="RsvpHexColor"/>
    /// map (not lifted, per events.md § Attendee list presentation).
    /// </summary>
    public string RsvpLabel =>
        Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.RsvpStatusLabel(Rsvp ?? string.Empty));

    /// <summary>
    /// Hex color string for the RSVP status label. Canonical color map aligned
    /// to android AttendeeRow + web statusColor (events.md § Attendee list
    /// presentation, 2026-06-23).
    /// </summary>
    public string RsvpHexColor => Rsvp?.ToLowerInvariant() switch
    {
        "going" or "accepted" => "#16A34A",        // green
        "interested" => "#CA8A04",                 // yellow-amber
        "declined" => "#DC2626",                   // red
        "waitlisted" => "#EA580C",                 // orange
        _ => "#6B7280",                            // secondary gray (invited / unknown)
    };
}
