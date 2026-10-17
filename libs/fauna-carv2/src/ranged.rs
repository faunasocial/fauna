//! CARv2 reader over an async ranged read — the [`crate::Reader`] for a medium
//! that answers only "these bytes at this offset", later.
//!
//! [`crate::Reader`] needs a synchronous `Read + Seek`, which some media cannot
//! give: a browser's OPFS file is read on the main thread only through
//! `File.slice().arrayBuffer()`, a promise. Reading such a file whole into a
//! `Cursor` would serve the sync reader, at the cost of holding the container
//! — which for a segment has no size ceiling. [`RangedReader`] reads the same
//! layout through [`ReadAt`] instead: the pragma and header, then the index
//! (held in memory, as [`crate::Reader`] holds it), then each record by its
//! indexed offset. What it holds is the index plus one record, never the
//! container.
//!
//! The byte decoding is shared with the sync reader ([`crate::v1`],
//! [`crate::index`]); only the fetching differs.

use crate::error::Error;
use crate::header::Header;
use crate::index::{
    INDEX_ENTRY_LEN, INDEX_PREFIX_MAX_LEN, IndexEntry, read_index_entries, read_index_prefix,
};
use crate::pragma::{Pragma, parse_pragma};
use crate::upstream_cid_to_fauna;
use crate::v1::{MAX_RECORD_LEN, decode_file_header, decode_record, read_varint_usize};
use fauna_cbor::Cid;
use std::io::{Cursor, Read, Seek, SeekFrom};

/// The most bytes an unsigned varint prefix occupies (a `usize` on a 64-bit
/// target).
const VARINT_MAX_LEN: u64 = 10;

/// A medium read by range — the same `read_at(offset, max_len)` shape as
/// `fauna_client::RangedNestReader`, which reads a nest file over HTTP `Range`.
#[allow(async_fn_in_trait)] // static dispatch only; no Send bound wanted (the web's is !Send)
pub trait ReadAt {
    /// Up to `max_len` bytes starting at `offset`: shorter only at the end of
    /// the medium, empty past it.
    async fn read_at(&mut self, offset: u64, max_len: u64) -> std::io::Result<Vec<u8>>;
}

impl<R: ReadAt + ?Sized> ReadAt for &mut R {
    async fn read_at(&mut self, offset: u64, max_len: u64) -> std::io::Result<Vec<u8>> {
        (**self).read_at(offset, max_len).await
    }
}

/// [`ReadAt`] over any synchronous `Read + Seek` — a file, a `Cursor`.
pub struct SeekReadAt<R>(pub R);

impl<R: Read + Seek> ReadAt for SeekReadAt<R> {
    async fn read_at(&mut self, offset: u64, max_len: u64) -> std::io::Result<Vec<u8>> {
        self.0.seek(SeekFrom::Start(offset))?;
        let mut out = Vec::new();
        (&mut self.0).take(max_len).read_to_end(&mut out)?;
        Ok(out)
    }
}

/// Exactly `len` bytes at `offset`, or `UnexpectedEof`.
async fn read_exact_at<R: ReadAt>(r: &mut R, offset: u64, len: u64) -> Result<Vec<u8>, Error> {
    let buf = r.read_at(offset, len).await?;
    if buf.len() as u64 != len {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!(
                "wanted {len} bytes at offset {offset}, the medium ends after {}",
                buf.len()
            ),
        )));
    }
    Ok(buf)
}

/// A varint-length-prefixed frame's body at `offset` (the CARv1 file header,
/// or one record), bounded by [`MAX_RECORD_LEN`]: two reads, the prefix then
/// the body.
async fn read_frame_at<R: ReadAt>(r: &mut R, offset: u64) -> Result<Vec<u8>, Error> {
    let prefix = r.read_at(offset, VARINT_MAX_LEN).await?;
    let mut cursor = Cursor::new(&prefix[..]);
    let len = read_varint_usize(&mut cursor)?;
    if len > MAX_RECORD_LEN {
        return Err(Error::BadIndex(format!(
            "frame length {len} exceeds MAX_RECORD_LEN {MAX_RECORD_LEN}"
        )));
    }
    read_exact_at(r, offset + cursor.position(), len as u64).await
}

/// CARv2 reader over a [`ReadAt`] — the async sibling of [`crate::Reader`].
pub struct RangedReader<R: ReadAt> {
    r: R,
    header: Header,
    /// Index entries, sorted ascending by digest (verified by
    /// [`read_index_entries`]).
    entries: Vec<IndexEntry>,
}

impl<R: ReadAt> RangedReader<R> {
    /// Validate the pragma, decode the header, check the CARv1 inner header
    /// and read the index — what [`crate::Reader::new`] does, by range.
    pub async fn new(mut r: R) -> Result<Self, Error> {
        let head = read_exact_at(&mut r, 0, (Pragma::LEN + Header::LEN) as u64).await?;
        parse_pragma(&head)?;
        let header_bytes: &[u8; Header::LEN] = head[Pragma::LEN..]
            .try_into()
            .expect("read exactly Pragma::LEN + Header::LEN bytes");
        let header = Header::decode(header_bytes)?;

        decode_file_header(&read_frame_at(&mut r, header.data_offset).await?)?;

        let prefix = r.read_at(header.index_offset, INDEX_PREFIX_MAX_LEN).await?;
        let mut cursor = Cursor::new(&prefix[..]);
        let count = read_index_prefix(&mut cursor)?;
        // The entry bytes are fetched before they are decoded, so a corrupt
        // count costs at most what the medium actually holds.
        let entry_bytes = read_exact_at(
            &mut r,
            header.index_offset + cursor.position(),
            count * INDEX_ENTRY_LEN,
        )
        .await?;
        let entries = read_index_entries(&mut &entry_bytes[..], count)?;

        Ok(RangedReader { r, header, entries })
    }

    /// The decoded 40-byte v2 header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The number of indexed blocks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The `i`th `(cid, block)` in index order (ascending by digest) — one
    /// [`crate::Reader::iter`] step: the record at the entry's offset, its
    /// on-disk digest checked against the entry. Like the sync reader, it
    /// never re-hashes the block. `None` past the end.
    pub async fn block_at(&mut self, i: usize) -> Option<Result<(Cid, Vec<u8>), Error>> {
        let entry = *self.entries.get(i)?;
        Some(self.read_entry(entry).await)
    }

    async fn read_entry(&mut self, entry: IndexEntry) -> Result<(Cid, Vec<u8>), Error> {
        let (on_disk_cid, block) = decode_record(&read_frame_at(&mut self.r, entry.offset).await?)?;
        let fauna = upstream_cid_to_fauna(&on_disk_cid)?;
        if fauna.digest() != entry.digest {
            return Err(Error::BadIndex(
                "index digest does not match on-disk CID digest".into(),
            ));
        }
        Ok((fauna, block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Reader, Writer};
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Drive a future that never waits — every medium in these tests answers
    /// synchronously.
    fn ready<F: Future>(f: F) -> F::Output {
        match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(out) => out,
            Poll::Pending => panic!("a synchronous medium never pends"),
        }
    }

    /// A [`ReadAt`] that remembers the largest read it answered.
    struct Counting<R> {
        inner: R,
        largest: u64,
    }

    impl<R: ReadAt> ReadAt for Counting<R> {
        async fn read_at(&mut self, offset: u64, max_len: u64) -> std::io::Result<Vec<u8>> {
            let out = self.inner.read_at(offset, max_len).await?;
            self.largest = self.largest.max(out.len() as u64);
            Ok(out)
        }
    }

    fn car(bodies: &[Vec<u8>]) -> (Vec<u8>, Vec<Cid>) {
        let mut w = Writer::new(Cursor::new(Vec::new()), &[]).unwrap();
        let cids: Vec<Cid> = bodies
            .iter()
            .map(|b| {
                let cid = Cid::of_dag_cbor(b);
                w.write_block(&cid, b).unwrap();
                cid
            })
            .collect();
        (w.finalize().unwrap().into_inner(), cids)
    }

    #[test]
    fn reads_what_the_sync_reader_reads_holding_one_record_at_a_time() {
        let bodies: Vec<Vec<u8>> = (0..16u8).map(|i| vec![i; 4096]).collect();
        let (bytes, cids) = car(&bodies);

        let mut sync = Reader::new(Cursor::new(&bytes[..])).unwrap();
        let expected: Vec<(Cid, Vec<u8>)> = sync.iter().map(Result::unwrap).collect();

        let mut ranged = ready(RangedReader::new(Counting {
            inner: SeekReadAt(Cursor::new(&bytes[..])),
            largest: 0,
        }))
        .unwrap();
        assert_eq!(ranged.len(), cids.len());
        assert_eq!(ranged.header(), sync.header());
        let got: Vec<(Cid, Vec<u8>)> = (0..ranged.len())
            .map(|i| ready(ranged.block_at(i)).unwrap().unwrap())
            .collect();
        assert_eq!(got, expected);
        assert!(ready(ranged.block_at(ranged.len())).is_none());

        // One record (4096 bytes + its 36-byte CID) is the largest read; the
        // container is 16 times that.
        assert!(
            ranged.r.largest <= 4096 + 36,
            "largest read {} — the reader fetched more than one record",
            ranged.r.largest
        );
        assert!(bytes.len() as u64 > 16 * 4096);
    }

    #[test]
    fn refuses_what_the_sync_reader_refuses() {
        let (bytes, _) = car(&[b"a".to_vec(), b"b".to_vec()]);

        let mut bad_pragma = bytes.clone();
        bad_pragma[0] ^= 0xff;
        assert!(matches!(
            ready(RangedReader::new(SeekReadAt(Cursor::new(bad_pragma)))),
            Err(Error::BadPragma)
        ));

        // Cut inside the index: the entries the prefix promises are not there.
        let truncated = bytes[..bytes.len() - 8].to_vec();
        assert!(Reader::new(Cursor::new(&truncated[..])).is_err());
        assert!(matches!(
            ready(RangedReader::new(SeekReadAt(Cursor::new(truncated)))),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn a_record_whose_digest_disagrees_with_its_entry_is_refused() {
        let (mut bytes, cids) = car(&[b"only".to_vec()]);
        // Flip the last byte of the on-disk CID's digest (the CID sits right
        // before the 4-byte body, which is the last record in the data payload).
        let mut ranged = ready(RangedReader::new(SeekReadAt(Cursor::new(bytes.clone())))).unwrap();
        let offset = ranged.entries[0].offset as usize;
        // varint(1 byte) + 36-byte CID: its final digest byte.
        bytes[offset + 36] ^= 0xff;
        ranged = ready(RangedReader::new(SeekReadAt(Cursor::new(bytes)))).unwrap();
        assert_eq!(ranged.len(), cids.len());
        assert!(matches!(
            ready(ranged.block_at(0)).unwrap(),
            Err(Error::BadIndex(_))
        ));
    }
}
