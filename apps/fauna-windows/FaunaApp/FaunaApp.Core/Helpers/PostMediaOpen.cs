using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Fetch a feed post's media blob and open it for rendering.
///
/// <para><b>Every post-image path goes through here, gated post or not.</b> A public
/// post's blob is plaintext on the wire and comes straight back; a tier-restricted
/// post's attachment is AEAD-sealed under the same per-post key its body opened under
/// and must be opened before the bytes are an image
/// (<c>docs/goal/ui/media.md</c> § Encryption at rest — one per-post key seals body and
/// attachments alike; <c>docs/goal/ui/feed.md</c> § Encryption at rest — "unsealing at
/// render time on the reader's client"). Routing every hash through the one shared-Rust
/// call is what keeps the post card free of an is-this-post-gated branch, which is why
/// this takes no "is it gated" argument and offers no second entry point.</para>
///
/// <para>The fetch stays app glue — the shared manager is WS-RPC-only and never pulls
/// bulk blob bytes itself, the same division <c>UnlockGatedPost</c> already draws.</para>
///
/// <para><b>The C2PA badge is the viewer's verdict over these bytes, never the uploader's
/// word</b> (<c>docs/goal/ui/media.md</c> § Encryption at rest → <i>C2PA provenance</i>).
/// The <c>x-c2pa</c> header on this same GET is the uploader's own assertion — the nest
/// stores <c>has_c2pa</c> for a public-post blob without inspecting the bytes — so it is
/// only a PRE-FILTER here: <c>false</c> ends the check (essentially every post), and
/// <c>true</c> is what buys a parse of the bytes the manager just opened, through the
/// one shared detector every app's badge agrees with (UniFFI
/// <c>detect_media_c2pa</c>, which takes bytes and deliberately no MIME). The bytes are
/// already in hand for the image itself, so unlike apple and web there is no extra
/// fetch.</para>
/// </summary>
internal static class PostMediaOpen
{
    /// <summary>
    /// GET the blob by hash, hand the bytes to <c>FfiFeedManager.OpenMediaBytes</c>, and —
    /// only when the <c>x-c2pa</c> pre-filter said <c>true</c> — take the C2PA verdict over
    /// the opened bytes.
    /// </summary>
    /// <param name="nest">The current nest client (the caller resolves it per call —
    /// see <c>BlobImageLoader</c>'s note on disposed clients).</param>
    /// <param name="manager">The session's feed manager, or <c>null</c> before the Feed
    /// page has built one. A <c>null</c> manager holds no per-post keys, so there is
    /// nothing it could open and the fetched bytes are returned unchanged — the same
    /// answer it would give for an unregistered hash.</param>
    /// <param name="hash">Content address of the blob to render.</param>
    /// <param name="detect">The verdict over opened bytes; <c>null</c> (every production
    /// caller) means the real shared detector, <c>FaunaFfiMethods.DetectMediaC2pa</c>.
    /// Injectable only so a test can observe what it was handed.</param>
    /// <returns>
    /// The bytes to decode, and the badge verdict. <c>Bytes</c> is <c>null</c> when
    /// the blob IS a sealed item of an unlocked post and did not open — the caller paints
    /// its existing placeholder, exactly as for bytes it cannot decode. It is never the
    /// ciphertext: handing undecodable AEAD bytes to a decoder would look identical to a
    /// corrupt image and hide a real key failure, and the detector is never handed it
    /// either. <c>HasC2pa</c> is <c>true</c> only when a parseable manifest is PRESENT in
    /// the opened bytes — presence, not validity or signer (<c>media.md</c> § What the
    /// badge certifies).
    /// </returns>
    internal static async Task<(byte[]? Bytes, bool HasC2pa)> FetchAndOpenAsync(
        INestHttpClient nest,
        IFfiFeedManager? manager,
        string hash,
        CancellationToken ct = default,
        Func<byte[], bool>? detect = null)
    {
        var (fetched, headerAssertsC2pa) = await nest.GetBlobWithC2paAsync(hash, ct);
        var opened = manager is null ? fetched : manager.OpenMediaBytes(hash, fetched);

        if (opened is null || !headerAssertsC2pa) return (opened, false);
        return (opened, await C2paVerdictAsync(opened, detect ?? FaunaFfiMethods.DetectMediaC2pa));
    }

    /// <summary>
    /// Run the detector off the caller's thread — every caller is a WinUI page on the UI
    /// thread, and a manifest parse over a full-size image is not a UI-thread job — and
    /// fail CLOSED. The badge is decoration on an image the reader can already see, so an
    /// exception out of the parse costs the badge, never the image: left to propagate it
    /// would reach <c>BlobImageLoader</c>'s swallow-and-skip and blank the whole card.
    /// The await resumes on the caller's context, as the GET above does.
    /// </summary>
    private static async Task<bool> C2paVerdictAsync(byte[] opened, Func<byte[], bool> detect)
    {
        try
        {
            return await Task.Run(() => detect(opened));
        }
        catch (Exception ex)
        {
            // Traced, not silent — the same standing BlobImageLoader's own catch has.
            System.Diagnostics.Debug.WriteLine($"PostMediaOpen: c2pa detect failed, no badge: {ex}");
            return false;
        }
    }
}
