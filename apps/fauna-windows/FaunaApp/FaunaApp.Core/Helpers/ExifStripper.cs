using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Strips privacy metadata (EXIF/IPTC) from image bytes before upload — lossless
/// container-segment removal, never a re-encode. Photo-library backup ingress must
/// preserve the exact bytes a restore returns, so this delegates to the shared
/// <c>fauna_media::process::strip_metadata</c> (via <c>FaunaFfiMethods.StripMediaMetadata</c>)
/// rather than round-tripping through <c>BitmapEncoder</c>, which re-encodes and
/// degrades the image even when there is no metadata to strip.
/// </summary>
public static class ExifStripper
{
    public static byte[] Strip(byte[] imageBytes) => FaunaFfiMethods.StripMediaMetadata(imageBytes);
}
