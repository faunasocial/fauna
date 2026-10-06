namespace FaunaApp.Core.Models;

public record CalendarInfo(string Id, string Name, string? Color, string? Timezone);

public record EventInfo(
    string Id,
    string Summary,
    DateTimeOffset Start,
    DateTimeOffset End,
    string? Description,
    string? Location,
    string? CalendarId,
    string? RsvpStatus,
    string? CalendarColor,
    bool IsAllDay = false)
{
    public string TimeRange
    {
        get
        {
            if (Start.Date == End.Date)
                return $"{Start:MMM d} {Start:h:mm tt} – {End:h:mm tt}";
            return $"{Start:MMM d h:mm tt} – {End:MMM d h:mm tt}";
        }
    }

    public string DayHeader => Start.ToString("dddd, MMMM d");
}
