"""Reading a `com.atproto.sync.subscribeRepos` firehose frame.

Shared by every test that asserts on what actually reached the network:
`test_atproto_firehose_post.py` (the outbound direction — a Fauna post reaches
the firehose) and `test_atproto_external_write.py` (the inbound direction — an
external app's write reaches it). Both need the same two things, so both get
them from one place: a frame is `[dag-cbor header][dag-cbor body]` back to back
in ONE binary WS message, and a `#commit`'s `blocks` field is a CARv1 whose
dag-cbor blocks carry the records themselves.

Reading the records out of the CAR (rather than trusting the `ops` paths) is
what makes a firehose assertion about CONTENT rather than about shape.
"""

import io

import cbor2


def read_varint(buf: io.BytesIO) -> int:
    shift = 0
    value = 0
    while True:
        byte = buf.read(1)
        if not byte:
            raise EOFError("truncated varint")
        value |= (byte[0] & 0x7F) << shift
        if not byte[0] & 0x80:
            return value
        shift += 7


def car_records(car: bytes) -> list[dict]:
    """Every dag-cbor block in a CARv1 that decodes to a record-shaped map.

    A minimal reader — enough to prove the frame itself carries the record,
    rather than trusting the ``ops`` path alone. Blocks that are not dag-cbor
    maps (MST nodes, the commit) are skipped.
    """
    buf = io.BytesIO(car)
    _ = buf.read(read_varint(buf))  # CAR header
    out: list[dict] = []
    while True:
        try:
            length = read_varint(buf)
        except EOFError:
            break
        block = buf.read(length)
        if not block:
            break
        # CIDv1 = 0x01 <varint codec> <multihash: varint code, varint len, digest>
        body = io.BytesIO(block)
        if body.read(1) != b"\x01":
            continue
        read_varint(body)  # codec
        read_varint(body)  # multihash code
        digest_len = read_varint(body)
        body.read(digest_len)
        try:
            value = cbor2.loads(body.read())
        except Exception:
            continue
        if isinstance(value, dict) and "$type" in value:
            out.append(value)
    return out


def decode_frame(message: bytes) -> tuple[dict, dict]:
    """A subscribeRepos frame is [dag-cbor header][dag-cbor body], back to back
    in ONE binary WS message."""
    buf = io.BytesIO(message)
    decoder = cbor2.CBORDecoder(buf)
    header = decoder.decode()
    body = decoder.decode()
    return header, body
