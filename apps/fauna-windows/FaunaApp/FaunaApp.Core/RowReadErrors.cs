using System.Collections.Generic;

namespace FaunaApp.Core;

/// <summary>Which of an open folder row's background reads a slot belongs to.</summary>
internal enum RowRead
{
    ActorMembers,
    DeviceActivity,
    PlaceRoster,
    DestinationPlaces,
}

/// <summary>
/// The error text each open folder row's background read has put up, one slot
/// per (read, set) — folders.md § Errors &amp; edge cases, "a folder row's own
/// reads retire their own error". A read's good answer clears exactly its own
/// slot, never a sibling read's and never a gesture's text (a gesture paints
/// the error bar directly and owns no slot). The page folds <see cref="Text"/>
/// into its one error-message bar; linux's <c>RowReadErrors</c> is the twin.
///
/// <para>A slot is dropped when its row closes or is rebuilt
/// (<see cref="ClearAll"/>), so a read that can no longer answer cannot leave
/// text up.</para>
/// </summary>
internal sealed class RowReadErrors
{
    // Insertion-ordered so the text shown is deterministic: the oldest standing
    // failure stays up until its own read answers.
    private readonly List<((RowRead Read, string Set) Key, string Text)> _slots = new();

    /// <summary>The read's failure text, replacing the same slot's earlier text.</summary>
    public void Set(RowRead read, string set, string text)
    {
        var key = (read, set);
        var i = _slots.FindIndex(s => s.Key == key);
        if (i >= 0) _slots[i] = (key, text);
        else _slots.Add((key, text));
    }

    /// <summary>The read answered: retire its own text. No-op without a slot.</summary>
    public void Clear(RowRead read, string set) =>
        _slots.RemoveAll(s => s.Key == (read, set));

    /// <summary>A row closed: drop every slot its set holds.</summary>
    public void ClearSet(string set) => _slots.RemoveAll(s => s.Key.Set == set);

    /// <summary>The rows were rebuilt: drop every slot of the page.</summary>
    public void ClearAll() => _slots.Clear();

    /// <summary>The text to fold into the error bar, or null when no read holds any.</summary>
    public string? Text => _slots.Count > 0 ? _slots[0].Text : null;
}
