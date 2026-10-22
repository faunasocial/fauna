//! Sequential walk of a CARv2 file's data section, without the index.
//!
//! A [`crate::Writer`] that was never finalized leaves a file with the pragma,
//! a zero-filled v2 header, the CARv1 file header and every block written so
//! far — but no index, so [`crate::Reader`] cannot open it. The data section is
//! written strictly in order, so walking it front to back recovers every block
//! the writer completed. [`scan_data_section`] does that walk and stops at the
//! first record that does not decode in full: a crash mid-`write_block` leaves
//! a torn final record, and power loss can leave zero-filled or garbage bytes
//! past the last durable one. Nothing after that point is a block.
//!
//! A finalized file (non-zero header) is walked too, bounded by the header's
//! `data_size`, so the index is never mistaken for a record — the segment
//! store's tail recovery re-runs over a file it rebuilt before a crash
//! (`fauna-segment-store`'s `FramedSegment::recover_unfinalized`).

use crate::error::Error;
use crate::header::Header;
use crate::pragma::Pragma;
use crate::upstream_cid_to_fauna;
use crate::v1::{MAX_RECORD_LEN, decode_record, read_file_header, read_varint_usize};
use fauna_cbor::Cid;
use std::io::{Read, Seek, SeekFrom};
use std::ops::ControlFlow;

/// How a [`scan_data_section`] walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataScan {
    /// Blocks handed to the callback.
    pub blocks: usize,
    /// Bytes between the last complete block and the end of the data section
    /// that did not decode as a record — a torn tail. `0` on a clean end, and
    /// on a walk the callback stopped.
    pub trailing_bytes: u64,
}

/// Walk the data section of the CARv2 file in `r` in write order, handing each
/// complete `(cid, block)` to `on_block` until the section ends, a record fails
/// to decode, or `on_block` returns `Break`.
///
/// Errors only when the file's prefix — pragma, v2 header, CARv1 file header —
/// is unreadable, or when `on_block` errors; a bad *record* ends the walk.
pub fn scan_data_section<R, F>(mut r: R, mut on_block: F) -> Result<DataScan, Error>
where
    R: Read + Seek,
    F: FnMut(Cid, Vec<u8>) -> Result<ControlFlow<()>, Error>,
{
    let file_len = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    let mut pragma = [0u8; Pragma::LEN];
    r.read_exact(&mut pragma)?;
    if pragma != Pragma::BYTES {
        return Err(Error::BadPragma);
    }
    let mut header_bytes = [0u8; Header::LEN];
    r.read_exact(&mut header_bytes)?;
    // A zero header is the unfinalized writer's placeholder: the data section
    // runs to the end of the file. Otherwise the header bounds it.
    let data_end = if header_bytes == [0u8; Header::LEN] {
        file_len
    } else {
        let header = Header::decode(&header_bytes)?;
        r.seek(SeekFrom::Start(header.data_offset))?;
        (header.data_offset + header.data_size).min(file_len)
    };
    read_file_header(&mut r)?;

    let mut blocks = 0;
    loop {
        let start = r.stream_position()?;
        if start >= data_end {
            return Ok(DataScan {
                blocks,
                trailing_bytes: 0,
            });
        }
        let Some((cid, block)) = read_bounded_record(&mut r, data_end) else {
            return Ok(DataScan {
                blocks,
                trailing_bytes: data_end - start,
            });
        };
        if on_block(cid, block)?.is_break() {
            blocks += 1;
            return Ok(DataScan {
                blocks,
                trailing_bytes: 0,
            });
        }
        blocks += 1;
    }
}

/// One record that lies wholly before `data_end` and decodes, or `None`.
fn read_bounded_record<R: Read + Seek>(r: &mut R, data_end: u64) -> Option<(Cid, Vec<u8>)> {
    let len = read_varint_usize(r).ok()?;
    let body_start = r.stream_position().ok()?;
    if len == 0 || len > MAX_RECORD_LEN || body_start + len as u64 > data_end {
        return None;
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    let (cid, block) = decode_record(&buf).ok()?;
    Some((upstream_cid_to_fauna(&cid).ok()?, block))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Writer;
    use std::io::{Cursor, Write};

    fn collect(bytes: Vec<u8>) -> (Vec<(Cid, Vec<u8>)>, DataScan) {
        let mut got = Vec::new();
        let scan = scan_data_section(Cursor::new(bytes), |cid, block| {
            got.push((cid, block));
            Ok(ControlFlow::Continue(()))
        })
        .expect("scan");
        (got, scan)
    }

    fn blocks() -> Vec<(Cid, Vec<u8>)> {
        (0..3u8)
            .map(|i| {
                let body = vec![i; 10 + i as usize];
                (Cid::of_dag_cbor(&body), body)
            })
            .collect()
    }

    /// The bytes an unfinalized writer leaves: no index, a zero header.
    fn unfinalized(blocks: &[(Cid, Vec<u8>)]) -> Vec<u8> {
        let mut w = Writer::new(Cursor::new(Vec::new()), &[]).expect("writer");
        for (cid, body) in blocks {
            w.write_block(cid, body).expect("write");
        }
        w.into_inner().into_inner()
    }

    #[test]
    fn an_unfinalized_file_yields_every_block_in_write_order() {
        let want = blocks();
        let (got, scan) = collect(unfinalized(&want));
        assert_eq!(got, want);
        assert_eq!(
            scan,
            DataScan {
                blocks: 3,
                trailing_bytes: 0
            }
        );
    }

    #[test]
    fn a_torn_final_record_ends_the_walk_before_it() {
        let want = blocks();
        let mut bytes = unfinalized(&want);
        let full = bytes.len();
        bytes.truncate(full - 4);
        let (got, scan) = collect(bytes);
        assert_eq!(got, want[..2]);
        assert_eq!(scan.blocks, 2);
        assert!(scan.trailing_bytes > 0, "{scan:?}");
    }

    #[test]
    fn zero_fill_past_the_last_block_is_not_a_record() {
        let want = blocks();
        let mut bytes = unfinalized(&want);
        bytes.write_all(&[0u8; 64]).expect("pad");
        let (got, scan) = collect(bytes);
        assert_eq!(got, want);
        assert_eq!(scan.trailing_bytes, 64);
    }

    #[test]
    fn a_finalized_file_is_walked_up_to_its_index_only() {
        let want = blocks();
        let mut w = Writer::new(Cursor::new(Vec::new()), &[]).expect("writer");
        for (cid, body) in &want {
            w.write_block(cid, body).expect("write");
        }
        let bytes = w.finalize().expect("finalize").into_inner();
        let (got, scan) = collect(bytes);
        assert_eq!(got, want);
        assert_eq!(scan.trailing_bytes, 0);
    }

    #[test]
    fn break_stops_the_walk() {
        let mut seen = 0;
        let scan = scan_data_section(Cursor::new(unfinalized(&blocks())), |_, _| {
            seen += 1;
            Ok(ControlFlow::Break(()))
        })
        .expect("scan");
        assert_eq!(seen, 1);
        assert_eq!(scan.blocks, 1);
    }
}
