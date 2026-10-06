import Foundation

/// The audience for a single client-side blob upload — the FFI-expressible
/// subset whose key material the client legitimately holds. Mirrors the
/// shared-Rust `FfiUploadAudience` (`libs/fauna-ffi/src/media_upload.rs`) and
/// the windows `UploadAudience`
/// (`apps/fauna-windows/.../Models/UploadAudience.cs`) — same concept on every
/// app (#1/#3).
///
/// The MLS-keyed audiences (`Conversation`, `RestrictedPost`) are deliberately
/// absent: their epoch / period secret lives in the shared-Rust MLS /
/// subscription state and must NOT be extracted across the FFI boundary. They
/// get a shared-Rust seal-by-id helper (tracked internally).
public enum UploadAudience: Equatable {
    /// Public-post-attached media — no seal; the bytes pass through as signed
    /// plaintext (the post's signature attests the blob hash).
    case publicPost
    /// Owner-only library media, sealed under the owner's 32-byte `BackupKey`
    /// (derived from the identity seed via `backupKeyDerive`).
    case library(backupKey: Data)
}

/// Map the client-uniform ``UploadAudience`` to the shared-Rust
/// `FfiUploadAudience` the seal pipeline consumes.
func mapUploadAudience(_ audience: UploadAudience) -> FfiUploadAudience {
    switch audience {
    case .publicPost:
        return .publicPost
    case .library(let backupKey):
        return .library(backupKey: backupKey)
    }
}

/// Resolve a file's MIME type from its extension for `AttachedFile.mediaType` /
/// `MediaItem.media_type` (`libs/fauna-feed/src/compose.rs`), via the shared
/// `content_type_for_filename` catalog (`libs/fauna-ffi/src/mime.rs`) — the
/// same door windows migrated its own hand-rolled `MimeDetect` onto, and the
/// same catalog `fauna_conversations::compose::guess_mime_type` wraps for
/// linux's native call. Never `UTType`-backed: the shared catalog is
/// intentionally the one canonical extension→MIME table every native app
/// resolves through, not the OS's (richer, but per-platform-divergent) UTI
/// database. Always resolves to a concrete string — `"application/octet-
/// stream"` for an unrecognized or missing extension, never nil. Used by the
/// compose-file attach path (test-injection today; the real native picker
/// will call the same helper).
func mimeType(forPath path: String) -> String {
    contentTypeForFilename(filename: path)
}

/// Build the `multipart/form-data` body for `POST /api/v1/blob` — exactly two
/// parts, `sidecar` (`application/cbor`, DAG-CBOR `UploadSidecar`) + `bytes`
/// (`application/octet-stream`, sealed bytes), in that order. Matches the nest's
/// `parse_multipart_upload` contract and the linux `post_multipart_blob` /
/// windows `BuildBlobMultipart` / android `postMultipartBlob` shape (#3). The
/// `boundary` is injected so the body is deterministic under test.
func buildBlobMultipart(sidecarCbor: Data, sealedBytes: Data, boundary: String) -> Data {
    var body = Data()
    func append(_ string: String) { body.append(Data(string.utf8)) }

    append("--\(boundary)\r\n")
    append("Content-Disposition: form-data; name=\"sidecar\"\r\n")
    append("Content-Type: application/cbor\r\n\r\n")
    body.append(sidecarCbor)
    append("\r\n--\(boundary)\r\n")
    append("Content-Disposition: form-data; name=\"bytes\"\r\n")
    append("Content-Type: application/octet-stream\r\n\r\n")
    body.append(sealedBytes)
    append("\r\n--\(boundary)--\r\n")
    return body
}
