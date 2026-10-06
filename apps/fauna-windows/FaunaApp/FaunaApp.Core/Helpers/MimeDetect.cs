using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// MIME content-type of a file path, derived from its extension. Delegates to the
/// single shared catalog in shared Rust
/// (<c>fauna_core::share::content_type_for_filename</c>, via the fauna-ffi
/// <c>mime</c> export) so every app resolves the same extension→MIME table
/// (priority #2/#4) — clients MUST NOT hand-roll it. Case-insensitive; returns
/// <c>"application/octet-stream"</c> for unknown or missing extensions.
/// </summary>
public static class MimeDetect
{
    public static string FromExtension(string path) =>
        FaunaFfiMethods.ContentTypeForFilename(path ?? string.Empty);
}
