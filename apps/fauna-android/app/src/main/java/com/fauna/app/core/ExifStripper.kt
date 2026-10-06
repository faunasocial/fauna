package com.fauna.app.core

import com.fauna.ffi.stripMediaMetadata

/**
 * Delegates to the shared `fauna_media::process::strip_metadata` (lossless
 * container segment removal, no decode/re-encode, C2PA preserved) instead of
 * the old Bitmap-decode-and-recompress path, which permanently degraded every
 * photo it touched. `mimeType` is unused by the shared face (it sniffs MIME
 * itself from the bytes) but kept in the signature to avoid touching every
 * call site.
 */
object ExifStripper {
    fun strip(bytes: ByteArray, mimeType: String): ByteArray {
        if (!mimeType.startsWith("image/")) return bytes
        return stripMediaMetadata(bytes)
    }
}
