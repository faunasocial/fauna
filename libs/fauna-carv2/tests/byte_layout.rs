//! Byte-layout tests for the CARv2 framing wrapper.
//!
//! These assert the literal bytes our writer produces against the CARv2
//! spec (https://ipld.io/specs/transport/car/carv2/) — pragma, header, and
//! `MultihashIndexSorted` index. The cross-language go-car/v2 oracle test
//! at Layer 3 Task 3.6 is the final end-to-end conformance gate.

use fauna_carv2::index::{
    BLAKE3_256_DIGEST_WIDTH, IndexEntry, MULTIHASH_BLAKE3_256, MULTIHASH_INDEX_SORTED_CODEC,
    write_index,
};
use fauna_carv2::{Error, Header, Pragma, Reader, Writer};
use fauna_cbor::Cid;
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Pragma
// ---------------------------------------------------------------------------

#[test]
fn pragma_is_eleven_bytes_and_matches_carv2_spec() {
    // The literal byte sequence per CARv2 spec § Pragma:
    //   0x0a                                       varint(10): payload length
    //   0xa1                                       dag-cbor map of 1 pair
    //   0x67 0x76 0x65 0x72 0x73 0x69 0x6f 0x6e    string "version" (7 bytes)
    //   0x02                                       uint(2)
    let expected: [u8; 11] = [
        0x0a, 0xa1, 0x67, 0x76, 0x65, 0x72, 0x73, 0x69, 0x6f, 0x6e, 0x02,
    ];
    assert_eq!(Pragma::BYTES.len(), 11);
    assert_eq!(Pragma::BYTES, expected);
    assert_eq!(Pragma::LEN, 11);
}

#[test]
fn parse_pragma_rejects_other_bytes() {
    let mut bad = Pragma::BYTES;
    bad[0] = 0xff;
    assert!(matches!(
        fauna_carv2::parse_pragma(&bad),
        Err(Error::BadPragma)
    ));
}

#[test]
fn parse_pragma_rejects_too_short() {
    assert!(matches!(
        fauna_carv2::parse_pragma(&[0x0a, 0xa1, 0x67]),
        Err(Error::BadPragma)
    ));
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

#[test]
fn header_round_trip() {
    let mut characteristics = [0u8; 16];
    characteristics[0] = 0x80;
    let header = Header {
        characteristics,
        data_offset: 51,
        data_size: 1024,
        index_offset: 1075,
    };
    let encoded = header.encode();
    assert_eq!(encoded.len(), 40);
    let decoded = Header::decode(&encoded).expect("decode");
    assert_eq!(decoded, header);
}

#[test]
fn header_field_layout_at_known_offsets() {
    let characteristics = [
        0x80, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let header = Header {
        characteristics,
        data_offset: 0x0123_4567_89ab_cdef,
        data_size: 0xfedc_ba98_7654_3210,
        index_offset: 0x0011_0011_0011_0011,
    };
    let encoded = header.encode();
    // Bytes 0..16: characteristics, in order.
    assert_eq!(&encoded[0..16], &characteristics);
    // Bytes 16..24: data_offset, little-endian.
    assert_eq!(&encoded[16..24], &0x0123_4567_89ab_cdefu64.to_le_bytes());
    // Bytes 24..32: data_size, little-endian.
    assert_eq!(&encoded[24..32], &0xfedc_ba98_7654_3210u64.to_le_bytes());
    // Bytes 32..40: index_offset, little-endian.
    assert_eq!(&encoded[32..40], &0x0011_0011_0011_0011u64.to_le_bytes());
}

#[test]
fn header_len_constant_is_40() {
    assert_eq!(Header::LEN, 40);
}

// ---------------------------------------------------------------------------
// MultihashIndexSorted
// ---------------------------------------------------------------------------

#[test]
fn multihash_index_sorted_round_trip() {
    // 5 distinct digests (unsorted on input — write_index sorts in place).
    let mut entries = vec![
        IndexEntry {
            digest: [0x33; 32],
            offset: 100,
        },
        IndexEntry {
            digest: [0x11; 32],
            offset: 200,
        },
        IndexEntry {
            digest: [0x55; 32],
            offset: 300,
        },
        IndexEntry {
            digest: [0x22; 32],
            offset: 400,
        },
        IndexEntry {
            digest: [0x44; 32],
            offset: 500,
        },
    ];

    let mut buf = Vec::new();
    write_index(&mut buf, &mut entries).expect("write_index");

    // Bucket header layout: [varint(0x0401)][u32 bucket_count=1]
    //                       [varint(0x1e)][u32 width=32][u64 count=5]
    //                       [(32 bytes digest)(8 bytes offset)] * 5
    //
    // 0x0401 encodes as two bytes: 0x81 0x08 (unsigned-varint).
    let mut p = 0;
    assert_eq!(buf[p], 0x81);
    p += 1;
    assert_eq!(buf[p], 0x08);
    p += 1;
    assert_eq!(&buf[p..p + 4], &1u32.to_le_bytes());
    p += 4;
    // 0x1e fits in one varint byte (high bit clear).
    assert_eq!(buf[p], 0x1e);
    p += 1;
    assert_eq!(&buf[p..p + 4], &BLAKE3_256_DIGEST_WIDTH.to_le_bytes());
    p += 4;
    assert_eq!(&buf[p..p + 8], &5u64.to_le_bytes());
    p += 8;
    // Entries appear in ascending-digest order.
    assert_eq!(&buf[p..p + 32], &[0x11; 32]);
    p += 32;
    assert_eq!(&buf[p..p + 8], &200u64.to_le_bytes());
    p += 8;
    assert_eq!(&buf[p..p + 32], &[0x22; 32]);
    p += 32;
    assert_eq!(&buf[p..p + 8], &400u64.to_le_bytes());
    p += 8;
    assert_eq!(&buf[p..p + 32], &[0x33; 32]);
    p += 32;
    assert_eq!(&buf[p..p + 8], &100u64.to_le_bytes());
    p += 8;
    assert_eq!(&buf[p..p + 32], &[0x44; 32]);
    p += 32;
    assert_eq!(&buf[p..p + 8], &500u64.to_le_bytes());
    p += 8;
    assert_eq!(&buf[p..p + 32], &[0x55; 32]);
    p += 32;
    assert_eq!(&buf[p..p + 8], &300u64.to_le_bytes());
    p += 8;
    assert_eq!(p, buf.len(), "no trailing bytes");

    // Round-trip via read_index.
    let mut cursor = Cursor::new(&buf);
    let read = fauna_carv2::index::read_index(&mut cursor).expect("read_index");
    assert_eq!(read.len(), 5);
    assert_eq!(read[0].digest, [0x11; 32]);
    assert_eq!(read[0].offset, 200);
    assert_eq!(read[4].digest, [0x55; 32]);
    assert_eq!(read[4].offset, 300);
}

#[test]
fn index_constants_match_spec() {
    assert_eq!(MULTIHASH_INDEX_SORTED_CODEC, 0x0401);
    assert_eq!(MULTIHASH_BLAKE3_256, 0x1e);
    assert_eq!(BLAKE3_256_DIGEST_WIDTH, 32);
}

// ---------------------------------------------------------------------------
// Writer + Reader round-trip
// ---------------------------------------------------------------------------

#[test]
fn writer_reader_round_trip_three_blocks() {
    let block_a = b"first canonical dag-cbor block";
    let block_b = b"second one, slightly longer payload of bytes";
    let block_c = b"c";
    let cid_a = Cid::of_dag_cbor(block_a);
    let cid_b = Cid::of_dag_cbor(block_b);
    let cid_c = Cid::of_dag_cbor(block_c);

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid_a, block_a).expect("write a");
        writer.write_block(&cid_b, block_b).expect("write b");
        writer.write_block(&cid_c, block_c).expect("write c");
        writer.finalize().expect("finalize");
    }
    buf.set_position(0);

    let mut reader = Reader::new(&mut buf).expect("Reader::new");
    assert_eq!(reader.len(), 3);

    let read_a = reader.get(&cid_a).expect("get a");
    assert_eq!(read_a, block_a);
    let read_b = reader.get(&cid_b).expect("get b");
    assert_eq!(read_b, block_b);
    let read_c = reader.get(&cid_c).expect("get c");
    assert_eq!(read_c, block_c);

    // Iterator walks in sorted-digest order (whatever that is for these
    // three CIDs). Collect and verify the set matches what we wrote.
    let mut seen: Vec<(Cid, Vec<u8>)> = reader
        .iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("iter ok");
    assert_eq!(seen.len(), 3);
    seen.sort_by_key(|(c, _)| *c.as_bytes());
    let mut expected = vec![
        (cid_a, block_a.to_vec()),
        (cid_b, block_b.to_vec()),
        (cid_c, block_c.to_vec()),
    ];
    expected.sort_by_key(|(c, _)| *c.as_bytes());
    assert_eq!(seen, expected);
}

#[test]
fn writer_finalize_writes_correct_header() {
    let block = b"hello world";
    let cid = Cid::of_dag_cbor(block);

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid, block).expect("write");
        writer.finalize().expect("finalize");
    }
    let bytes = buf.into_inner();

    // First 11 bytes are the pragma.
    assert_eq!(&bytes[..11], &Pragma::BYTES);

    // Bytes 11..51 are the header.
    let mut header_bytes = [0u8; 40];
    header_bytes.copy_from_slice(&bytes[11..51]);
    let header = Header::decode(&header_bytes).expect("decode header");
    assert_eq!(header.data_offset, 51);
    // The data section spans from data_offset to index_offset.
    assert_eq!(header.data_offset + header.data_size, header.index_offset);
    // The index spans from index_offset to end-of-file.
    assert!(header.index_offset < bytes.len() as u64);

    // Fully-indexed characteristics bit is set.
    assert_eq!(header.characteristics[0] & 0x80, 0x80);

    // A second Reader reads it end to end.
    let mut cursor = Cursor::new(bytes);
    let mut reader = Reader::new(&mut cursor).expect("Reader::new");
    assert_eq!(reader.len(), 1);
    assert_eq!(reader.get(&cid).expect("get"), block);
}

#[test]
fn writer_accepts_empty_roots() {
    // Segment-store calls with no semantic root. The CARv1 spec allows an
    // empty roots array; fauna-carv2 mustn't reject it.
    let mut buf = Cursor::new(Vec::<u8>::new());
    let writer = Writer::new(&mut buf, &[]).expect("Writer::new with empty roots");
    writer.finalize().expect("finalize zero-block file");

    buf.set_position(0);
    let reader = Reader::new(&mut buf).expect("Reader::new for zero-block file");
    assert_eq!(reader.len(), 0);
    assert!(reader.is_empty());
}

#[test]
fn writer_accepts_single_root() {
    // Manifest-style call with one root.
    let block = b"the root block";
    let root_cid = Cid::of_dag_cbor(block);

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[&root_cid]).expect("Writer::new");
        writer.write_block(&root_cid, block).expect("write root");
        writer.finalize().expect("finalize");
    }
    buf.set_position(0);
    let mut reader = Reader::new(&mut buf).expect("Reader::new");
    assert_eq!(reader.get(&root_cid).expect("get"), block);
}

// ---------------------------------------------------------------------------
// Reader rejects invalid files
// ---------------------------------------------------------------------------

#[test]
fn reader_rejects_bad_pragma() {
    let mut bytes = vec![0u8; 200];
    bytes[0] = 0xff; // not the pragma
    let cursor = Cursor::new(bytes);
    let res = Reader::new(cursor);
    assert!(matches!(res, Err(Error::BadPragma)));
}

#[test]
fn reader_rejects_v1_with_wrong_version() {
    // Build a file by hand: pragma + header + a forged v1 header with
    // version=2. Index can be a real index with zero entries.
    use serde::Serialize;

    #[derive(Serialize)]
    struct ForgedHeader {
        roots: Vec<cid::Cid>,
        version: u64,
    }

    let mut buf = Vec::new();
    buf.extend_from_slice(&Pragma::BYTES);

    let data_offset = buf.len() as u64; // 11

    // Placeholder for the 40-byte header; we'll patch it after we know offsets.
    buf.extend_from_slice(&[0u8; 40]);
    let after_header = buf.len() as u64; // 51

    // Forged v1 header with version=2.
    let forged = ForgedHeader {
        roots: vec![],
        version: 2,
    };
    let cbor = serde_ipld_dagcbor::to_vec(&forged).unwrap();
    let mut vbuf = unsigned_varint::encode::usize_buffer();
    let v = unsigned_varint::encode::usize(cbor.len(), &mut vbuf);
    buf.extend_from_slice(v);
    buf.extend_from_slice(&cbor);

    let index_offset = buf.len() as u64;
    // Write an empty MultihashIndexSorted (0 entries).
    let mut bucket_buf = Vec::new();
    let mut vbuf2 = unsigned_varint::encode::u64_buffer();
    bucket_buf.extend_from_slice(unsigned_varint::encode::u64(0x0401, &mut vbuf2));
    bucket_buf.extend_from_slice(&1u32.to_le_bytes());
    bucket_buf.extend_from_slice(unsigned_varint::encode::u64(0x1e, &mut vbuf2));
    bucket_buf.extend_from_slice(&32u32.to_le_bytes());
    bucket_buf.extend_from_slice(&0u64.to_le_bytes());
    buf.extend_from_slice(&bucket_buf);

    // Patch the header in place.
    let header = Header {
        characteristics: [0u8; 16],
        data_offset,
        data_size: index_offset - data_offset,
        index_offset,
    };
    buf[11..51].copy_from_slice(&header.encode());

    let cursor = Cursor::new(buf);
    let res = Reader::new(cursor);
    let _ = after_header;
    match &res {
        Err(Error::BadV1Header(_)) => {}
        Ok(_) => panic!("expected BadV1Header, got Ok"),
        Err(other) => panic!("expected BadV1Header, got {other:?}"),
    }
}

#[test]
fn reader_rejects_block_with_corrupted_cid_prefix() {
    // Write a normal file, then flip a byte in the on-disk CID at the
    // block's offset (NOT in the index). The reader's get() compares the
    // on-disk CID bytes to the requested CID and returns CidMismatch.
    let block = b"block bytes that are stable";
    let cid = Cid::of_dag_cbor(block);

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid, block).expect("write");
        writer.finalize().expect("finalize");
    }
    let mut bytes = buf.into_inner();

    // After the file is finalized, find the block's offset from the index.
    // For our 1-block file, the block starts at data_offset + v1-header-length;
    // we don't need to compute that — just scan for the CID's 4-byte prefix
    // (0x01 0x71 0x1e 0x20) followed by 32 digest bytes and flip the version
    // byte (first byte of the CID prefix) in the *data section*, not the
    // index digest.
    let prefix = [0x01u8, 0x71, 0x1e, 0x20];
    // Find the prefix in the data section, before the index.
    let mut header_bytes = [0u8; 40];
    header_bytes.copy_from_slice(&bytes[11..51]);
    let header = Header::decode(&header_bytes).unwrap();
    let data_range = (header.data_offset as usize)..(header.index_offset as usize);
    let data = &bytes[data_range.clone()];
    let pos = data
        .windows(prefix.len())
        .position(|w| w == prefix)
        .expect("CID prefix appears in data section");
    let absolute = data_range.start + pos;
    // Corrupt the codec byte (turns 0x71 into 0x72 — upstream_cid_to_fauna
    // rejects it).
    bytes[absolute + 1] = 0x72;

    let mut cursor = Cursor::new(bytes);
    let mut reader = Reader::new(&mut cursor).expect("Reader::new still works");
    let res = reader.get(&cid);
    // The on-disk CID's codec is wrong → upstream_cid_to_fauna fails with
    // UnsupportedCodec; the round-trip semantically detected corruption.
    assert!(matches!(res, Err(Error::UnsupportedCodec(0x72))));
}

#[test]
fn block_len_matches_payload_length_without_reading_body() {
    // The size source for IMAP RFC822.SIZE / SEARCH LARGER|SMALLER /
    // STORAGE quota: the per-record block length looked up by CID through
    // the index, NOT a mirrored SQL column. block_len must return exactly
    // the byte length of the payload that was written (= cid_len+block_len
    // record frame minus the on-disk CID header).
    let block_a = b"first canonical dag-cbor block";
    let block_b = b"second one, slightly longer payload of bytes";
    let block_c = b"c";
    let cid_a = Cid::of_dag_cbor(block_a);
    let cid_b = Cid::of_dag_cbor(block_b);
    let cid_c = Cid::of_dag_cbor(block_c);

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid_a, block_a).expect("write a");
        writer.write_block(&cid_b, block_b).expect("write b");
        writer.write_block(&cid_c, block_c).expect("write c");
        writer.finalize().expect("finalize");
    }
    buf.set_position(0);

    let mut reader = Reader::new(&mut buf).expect("Reader::new");
    assert_eq!(
        reader.block_len(&cid_a).expect("len a"),
        block_a.len() as u64
    );
    assert_eq!(
        reader.block_len(&cid_b).expect("len b"),
        block_b.len() as u64
    );
    assert_eq!(
        reader.block_len(&cid_c).expect("len c"),
        block_c.len() as u64
    );
}

#[test]
fn block_len_returns_not_found_for_unknown_cid() {
    let block_a = b"a";
    let cid_a = Cid::of_dag_cbor(block_a);
    let cid_other = Cid::of_dag_cbor(b"not in the file");

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid_a, block_a).expect("write");
        writer.finalize().expect("finalize");
    }
    buf.set_position(0);
    let mut reader = Reader::new(&mut buf).expect("Reader::new");
    assert!(matches!(
        reader.block_len(&cid_other),
        Err(Error::CidNotFound)
    ));
}

#[test]
fn block_len_detects_corrupted_cid_prefix() {
    // Same corruption as reader_rejects_block_with_corrupted_cid_prefix:
    // block_len reads the on-disk CID header (to know its length) and so
    // must surface the same UnsupportedCodec error, not silently mis-size.
    let block = b"block bytes that are stable";
    let cid = Cid::of_dag_cbor(block);

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid, block).expect("write");
        writer.finalize().expect("finalize");
    }
    let mut bytes = buf.into_inner();

    let prefix = [0x01u8, 0x71, 0x1e, 0x20];
    let mut header_bytes = [0u8; 40];
    header_bytes.copy_from_slice(&bytes[11..51]);
    let header = Header::decode(&header_bytes).unwrap();
    let data_range = (header.data_offset as usize)..(header.index_offset as usize);
    let data = &bytes[data_range.clone()];
    let pos = data
        .windows(prefix.len())
        .position(|w| w == prefix)
        .expect("CID prefix appears in data section");
    let absolute = data_range.start + pos;
    bytes[absolute + 1] = 0x72;

    let mut cursor = Cursor::new(bytes);
    let mut reader = Reader::new(&mut cursor).expect("Reader::new still works");
    let res = reader.block_len(&cid);
    assert!(matches!(res, Err(Error::UnsupportedCodec(0x72))));
}

#[test]
fn block_len_rejects_frame_shorter_than_cid_header() {
    // block_len computes block_len = total_record_len - cid_len via
    // `checked_sub`. If a corrupted on-disk length-prefix varint declares a
    // total record length SMALLER than the CID header it then reads, the
    // subtraction underflows; block_len must surface `Error::BadIndex` (not
    // panic, not silently return a wrong size — this is the IMAP RFC822.SIZE /
    // quota source, so a wrong size would be a quota-bypass bug).
    let block = b"sizeable payload bytes, definitely no cid prefix in here";
    let cid = Cid::of_dag_cbor(block);

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid, block).expect("write");
        writer.finalize().expect("finalize");
    }
    let mut bytes = buf.into_inner();

    // Locate the record's on-disk CID header within the data section. The
    // record frame is `varint(cid_len + block_len) || cid_bytes || block`, and
    // the total here (36-byte CID + a sub-128-byte block) fits in a one-byte
    // varint, so the varint sits at exactly `cid_prefix_pos - 1`.
    let prefix = [0x01u8, 0x71, 0x1e, 0x20];
    let mut header_bytes = [0u8; 40];
    header_bytes.copy_from_slice(&bytes[11..51]);
    let header = Header::decode(&header_bytes).unwrap();
    let data_range = (header.data_offset as usize)..(header.index_offset as usize);
    let data = &bytes[data_range.clone()];
    let pos = data
        .windows(prefix.len())
        .position(|w| w == prefix)
        .expect("CID prefix appears in data section");
    let absolute = data_range.start + pos;
    // Overwrite the length-prefix varint with 10 (< the 36-byte CID header).
    // Still a one-byte varint (< 128), so no bytes shift; the CID and block
    // bytes are untouched, so the CID still decodes and matches the request.
    bytes[absolute - 1] = 0x0a;

    let mut cursor = Cursor::new(bytes);
    let mut reader = Reader::new(&mut cursor).expect("Reader::new still works");
    let res = reader.block_len(&cid);
    assert!(
        matches!(res, Err(Error::BadIndex(_))),
        "expected BadIndex for an underflowing frame length, got {res:?}"
    );
}

#[test]
fn reader_get_returns_not_found_for_unknown_cid() {
    let block_a = b"a";
    let cid_a = Cid::of_dag_cbor(block_a);
    let cid_other = Cid::of_dag_cbor(b"not in the file");

    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid_a, block_a).expect("write");
        writer.finalize().expect("finalize");
    }
    buf.set_position(0);
    let mut reader = Reader::new(&mut buf).expect("Reader::new");
    assert!(matches!(reader.get(&cid_other), Err(Error::CidNotFound)));
}

#[test]
fn header_placeholder_is_backfilled_in_place() {
    // Confirm finalize seeks back: the bytes at 11..51 after finalize are NOT
    // all zero (the placeholder was overwritten).
    let block = b"x";
    let cid = Cid::of_dag_cbor(block);
    let mut buf = Cursor::new(Vec::<u8>::new());
    {
        let mut writer = Writer::new(&mut buf, &[]).expect("Writer::new");
        writer.write_block(&cid, block).expect("write");
        writer.finalize().expect("finalize");
    }
    let bytes = buf.into_inner();
    let header_slice = &bytes[11..51];
    assert!(
        header_slice.iter().any(|b| *b != 0),
        "header placeholder should have been backfilled"
    );
}
