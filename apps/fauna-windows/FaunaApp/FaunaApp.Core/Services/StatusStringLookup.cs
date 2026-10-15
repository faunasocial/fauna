using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The app's own string catalog handed to the shared <c>StatusText</c> projection
/// (<c>ui/status.md</c> § State &amp; data shape). Returns the localized template for
/// <c>key</c> — its <c>{name}</c> tokens intact, the shared projection fills them —
/// or null when the catalog has no such key (the projection then falls back to the
/// key itself, so a miss surfaces rather than rendering blank). The key is dotted in
/// Rust; the generated resw is flat slash notation, so dots → slashes for the lookup
/// (the same mapping <see cref="Strings.Resolve"/> applies).
/// </summary>
internal sealed class StatusStringLookup : FfiStatusLookup
{
    public string? Lookup(string key)
    {
        var slashKey = key.Replace('.', '/');
        var template = Strings.Get(slashKey);
        // Strings.Get answers the key itself when it has none.
        return template == slashKey ? null : template;
    }
}
