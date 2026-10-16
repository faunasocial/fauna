namespace FauiBridge;

/// <summary>
/// The per-key reading of a HelpText that carries SEVERAL attributes. UIA exposes one
/// application-supplied string per element (<c>AutomationProperties.HelpText</c>), and
/// <see cref="Actions"/> answers every non-special attribute with it; an element that must
/// publish more than one — the <c>video-thumbnail</c> inline player's <c>state</c>,
/// <c>position</c> and <c>source</c> (<c>render-model.md</c> § D6c → <i>Inline
/// playback</i>) — packs them as <c>key=value</c> pairs joined by <c>;</c>, and this answers
/// one key's value, so <c>get_attr(id, "position")</c> reads the same on windows as on
/// every other app.
///
/// <para>The app-side writer is <c>FaunaApp.Core.Helpers.AutomationAttributePack.Pack</c>
/// — mirrored here, not shared: the bridge is a harness binary that links no app code. Its
/// grammar, which this reader holds exactly: two or more pairs; each key lowercase kebab
/// (<c>[a-z][a-z0-9-]*</c>); a value runs from its key's first <c>=</c> to the next
/// <c>;</c>, with <c>%</c> written <c>%25</c> and <c>;</c> written <c>%3B</c>; no trailing
/// <c>;</c>.</para>
///
/// <para>Strict on purpose: anything else is NOT a pack and the caller keeps the HelpText
/// verbatim, because many elements publish one bare value there (<c>painted</c>,
/// <c>true</c>, an id that may end in base64 <c>=</c>). A single pair is not a pack either
/// — one value needs no packing — which is also what keeps such an id from being misread.
/// Pinned by <see cref="SelfTest.PackedAttrChecks"/> (<c>--self-test-packed-attr</c>).</para>
/// </summary>
internal static class PackedAttr
{
    /// <summary>
    /// <c>true</c> when <paramref name="helpText"/> is a pack, with <paramref name="value"/>
    /// the unescaped value of <paramref name="key"/> — or <c>null</c> when the pack names no
    /// such key (the attribute is not set). <c>false</c> when it is not a pack: the caller
    /// answers the HelpText as it is.
    /// </summary>
    public static bool TryRead(string? helpText, string key, out string? value)
    {
        value = null;
        if (string.IsNullOrEmpty(helpText)) return false;
        var pairs = helpText.Split(';');
        if (pairs.Length < 2) return false;

        string? found = null;
        foreach (var pair in pairs)
        {
            var eq = pair.IndexOf('=');
            if (eq <= 0 || !IsKey(pair.AsSpan(0, eq))) return false;
            if (found is null && pair.AsSpan(0, eq).SequenceEqual(key.AsSpan()))
                found = Unescape(pair[(eq + 1)..]);
        }
        value = found;
        return true;
    }

    private static bool IsKey(ReadOnlySpan<char> key)
    {
        if (key.Length == 0 || key[0] is < 'a' or > 'z') return false;
        foreach (var c in key)
        {
            if (c is not ((>= 'a' and <= 'z') or (>= '0' and <= '9') or '-')) return false;
        }
        return true;
    }

    // `%3B` before `%25`: the writer escaped `%` first, so a literal "%3B" in a value was
    // written "%253B" — undoing `%25` first would turn that back into a `;`.
    private static string Unescape(string value)
        => value.Replace("%3B", ";").Replace("%25", "%");
}
