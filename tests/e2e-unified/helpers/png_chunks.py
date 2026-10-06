"""PNG chunk-stream helpers for the metadata-strip witnesses.

Lifted from ``tests/test_feed.py`` (2026-09-21) when the conversation-attachment strip
witness needed the same fixture builder and the same structural read: a photo posted to
the feed and a file attached to a mail both pass through the one shared strip
(``fauna_media::process::strip_metadata``), so both witnesses build their GPS-tagged
fixture and read the result back the same way.
"""

from __future__ import annotations

import zlib

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"

# The location a phone would have written into the photo. Its only job is to be
# unmistakable in a byte scan, so it is spelled as coordinates rather than as an
# opaque token — a failure prints what leaked, not just that something did.
EXIF_GPS_CANARY = b"canary-gps-51.5007N-0.1246W-do-not-leak"

# The ancillary chunk types that carry metadata a strip must remove.
METADATA_CHUNK_TYPES = (b"eXIf", b"tEXt", b"iTXt", b"zTXt")


def png_chunks(png_bytes: bytes) -> list[tuple[bytes, bytes]]:
    """Walk a PNG's chunk stream, returning ``(type, data)`` for each chunk.

    Structural on purpose. ``b"eXIf" in png_bytes`` is *not* the same question:
    four arbitrary bytes can land inside a deflate-compressed IDAT by accident,
    and a privacy assertion that a coin flip can decide is not an assertion. A
    four-byte type is a chunk type only when it sits at a real chunk boundary,
    which is what this walk establishes.
    """
    assert png_bytes[:8] == PNG_MAGIC, f"not a PNG: first bytes={png_bytes[:8]!r}"
    chunks: list[tuple[bytes, bytes]] = []
    offset = 8
    while offset + 12 <= len(png_bytes):
        length = int.from_bytes(png_bytes[offset : offset + 4], "big")
        ctype = png_bytes[offset + 4 : offset + 8]
        chunks.append((ctype, png_bytes[offset + 8 : offset + 8 + length]))
        offset += 12 + length  # length + type + data + CRC
        if ctype == b"IEND":
            break
    return chunks


def png_with_chunk(png_bytes: bytes, ctype: bytes, data: bytes) -> bytes:
    """Insert one ancillary PNG chunk carrying ``data`` just after IHDR.

    Built here rather than shipped as a binary fixture so what rides in the
    chunk is visible in the source that asserts on it (the same reason
    `fauna-media`'s own fixtures are programmatic).
    """
    assert png_bytes[:8] == PNG_MAGIC, f"not a PNG: first bytes={png_bytes[:8]!r}"
    assert len(ctype) == 4, f"a PNG chunk type is 4 bytes, got {ctype!r}"
    ihdr_length = int.from_bytes(png_bytes[8:12], "big")
    insert_at = 8 + 12 + ihdr_length  # signature + the whole IHDR chunk
    chunk = (
        len(data).to_bytes(4, "big")
        + ctype
        + data
        + zlib.crc32(ctype + data).to_bytes(4, "big")
    )
    return png_bytes[:insert_at] + chunk + png_bytes[insert_at:]


def png_with_exif_chunk(png_bytes: bytes, canary: bytes) -> bytes:
    """Insert an ancillary ``eXIf`` chunk carrying ``canary`` just after IHDR.

    The Python twin of ``libs/fauna-media/tests/process_test.rs``'s
    ``inject_png_exif_chunk`` — same chunk id, same role: ``eXIf`` is where a
    camera writes the Exif block that holds GPS.
    """
    return png_with_chunk(png_bytes, b"eXIf", canary)


def ihdr(png_bytes: bytes) -> bytes:
    """The IHDR payload — width, height, bit depth, colour type: "the same picture"."""
    return next(data for ctype, data in png_chunks(png_bytes) if ctype == b"IHDR")
