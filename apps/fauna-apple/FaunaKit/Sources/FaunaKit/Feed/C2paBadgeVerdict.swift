import Foundation

/// The viewer's `c2pa-badge` verdict over bytes it has already fetched
/// (`docs/goal/ui/media.md` § Encryption at rest → *C2PA provenance*).
///
/// The `x-c2pa` response header is the **uploader's own assertion**: the nest
/// stores `has_c2pa` for a public-post blob without ever inspecting the bytes, so
/// anyone posting through a modified client can make it `true` for an image that
/// carries no manifest. The badge is therefore the *viewer's* verdict, never the
/// header's. The header survives only as a pre-filter
/// (``APIClient/hasC2paAssertion(hash:)``): `false` ends the check without
/// fetching anything, `true` is what buys the byte-level parse this type owns —
/// the same two stages as tui's `Op::FetchC2pa`.
///
/// Pure, with its collaborators injected, for the reason
/// ``PostImageSource/resolve(isSealed:blobURL:opened:)`` is: the composition is
/// short and easy to get subtly wrong, and getting it wrong paints a provenance
/// claim nobody checked. `FeedVM.hasC2pa(_:)` supplies the manager and the fetch.
public enum C2paBadgeVerdict {
    /// - Parameters:
    ///   - fetched: the blob exactly as the nest served it.
    ///   - open: the shared manager's `open_media_bytes` seam
    ///     (``FfiFeedManager/openMediaBytes(blobHash:fetched:)``). Every hash goes
    ///     through it, unconditionally, with no is-this-post-restricted branch:
    ///     an unregistered hash — public media — comes back untouched, a sealed
    ///     item is unsealed under the key that opened its post, and one that
    ///     cannot be opened is `nil`. tui does the same before its own verdict,
    ///     and the detector must never be handed AEAD ciphertext.
    ///   - detect: the verdict itself. The default is the single UniFFI face of
    ///     `fauna_media::process::detect_c2pa_in_bytes` — the one function every
    ///     app's badge is meant to agree with. It takes bytes and deliberately no
    ///     MIME (an uploader-asserted MIME is as forgeable as the flag). Injectable
    ///     only so a test can observe what it was handed.
    /// - Returns: `true` / `false` — a completed verdict, a parseable manifest is
    ///   present in these bytes or it is not (what the badge certifies is *presence*,
    ///   not validity or signer — `media.md` § C2PA provenance). `nil` when `open`
    ///   refused the bytes: there was nothing to verify, which is not "verified
    ///   absent" — the caller paints no badge but must not remember it.
    public static func over(_ fetched: Data,
                            open: (Data) -> Data?,
                            detect: (Data) -> Bool = { detectMediaC2pa(raw: $0) }) -> Bool? {
        guard let plain = open(fetched) else { return nil }
        return detect(plain)
    }
}
