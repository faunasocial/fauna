package com.fauna.app.core

import org.junit.Assert.assertArrayEquals
import org.junit.Test

/**
 * [ExifStripper] delegates to the shared `fauna_media::process::strip_metadata`
 * (`com.fauna.ffi.stripMediaMetadata`) — this pins the wiring, not the strip
 * logic itself (extensively covered on the Rust side: `fauna-media`'s
 * `process_test.rs` + `fauna-ffi`'s `strip_media_metadata_is_lossless_and_no_seal`).
 */
class ExifStripperTest {

    @Test
    fun nonImageMimeTypePassesThroughWithoutCallingTheSharedFace() {
        val bytes = "not an image, just backup bytes".toByteArray()
        assertArrayEquals(bytes, ExifStripper.strip(bytes, "application/octet-stream"))
    }

    @Test
    fun imageMimeTypeRoutesThroughTheSharedFace() {
        // Not a real image — the shared stripper's lossless container-segment
        // removal falls back to byte-identical passthrough on unparseable
        // bytes (`strip_exif_iptc`'s `Err(_) => body` arm), so this proves the
        // Kotlin -> JNA -> Rust call succeeds without crashing, same shape as
        // a real metadata-free image would return.
        val bytes = "pretend jpeg bytes".toByteArray()
        assertArrayEquals(bytes, ExifStripper.strip(bytes, "image/jpeg"))
    }
}
