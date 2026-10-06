package com.fauna.app.core

/**
 * Hex encoding/decoding utilities for converting between hex strings stored in
 * SecureStorage and the raw ByteArrays expected by the UniFFI-generated bindings.
 */
object HexUtil {
    fun hexToBytes(hex: String): ByteArray =
        hex.chunked(2).map { it.toInt(16).toByte() }.toByteArray()

    fun bytesToHex(bytes: ByteArray): String =
        bytes.joinToString("") { "%02x".format(it) }
}
