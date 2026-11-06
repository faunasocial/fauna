namespace FaunaApp.Core;

/// <summary>What a page should do to its error bar.</summary>
internal enum BarAction
{
    /// <summary>Touch nothing.</summary>
    Leave,
    /// <summary>Paint the given message and open the bar.</summary>
    Show,
    /// <summary>Close the bar.</summary>
    Close,
}

/// <summary>
/// Who put the text on a page's one error-message bar. The page's chain
/// (machine error, row-read slots, …) paints through <see cref="Next"/>; a
/// gesture paints the bar directly and owns no slot. The bar is closed only
/// while it still shows text the chain itself put up, so a machine tick (a
/// <c>Resync</c>) that finds the chain empty never wipes a gesture's text.
/// </summary>
internal sealed class ErrorBarOwnership
{
    private string? _owned;

    /// <param name="chain">The chain's current text, or null when it holds none.</param>
    /// <param name="barOpen">Whether the bar is open right now.</param>
    /// <param name="barMessage">The text the bar shows right now.</param>
    public BarAction Next(string? chain, bool barOpen, string? barMessage)
    {
        if (chain is not null)
        {
            _owned = chain;
            return BarAction.Show;
        }
        var owned = _owned;
        _owned = null;
        return owned is not null && barOpen && barMessage == owned
            ? BarAction.Close
            : BarAction.Leave;
    }
}
