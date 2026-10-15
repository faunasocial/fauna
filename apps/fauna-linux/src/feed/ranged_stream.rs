//! The linux half of the **native-player leg** — a seekable `gio::InputStream`
//! over the shared pinned [`RangedNestReader`] (`security.md` § Transport trust
//! → *The native-player leg*; `render-model.md` § D6c → *Inline playback*,
//! answer (1)).
//!
//! `gtk::Video` plays through GStreamer, whose HTTP source trusts the OS store
//! alone — no loopback carve-out, no SPKI pin — so handed a nest URL it refuses
//! every nest on its self-signed floor, the same-box install included. So the
//! player is never handed a URL: `gtk::MediaFile::for_input_stream` reads this
//! stream, and every byte of it is a `Range` GET through the reader, over the
//! same `pinned_http_client` the content API uses. GStreamer's stream source
//! seeks a `GSeekable`, which is why the stream implements one.
//!
//! The rules live in [`RangedCursor`] (pure, unit-tested): the position, the
//! seeks, the known-length clamp, the read-on past a short answer — windows'
//! `RangedNestCursor` keeps the same ones — plus a **read-ahead window**:
//! GStreamer reads in 4 KiB blocks, and one `Range` GET per block would turn a
//! 5 MB clip into 1,200 round trips, so a read below [`READ_AHEAD`] fetches
//! that much and serves the following reads from the window.
//!
//! **Threading:** a read blocks on a network round trip. GStreamer calls it on
//! its own streaming thread, which is outside the tokio runtime, so the reader's
//! async calls are driven with [`tokio::runtime::Handle::block_on`] there —
//! never on the GTK main thread, which no caller here reads from.

use std::io::SeekFrom;
use std::sync::{Arc, Mutex, OnceLock};

use fauna_client::{RangedNestReader, RangedReadError};
use gtk::gio;
use gtk::glib;
use gtk::subclass::prelude::*;

/// The smallest window one `Range` GET fetches.
pub const READ_AHEAD: u64 = 256 * 1024;

/// Why a seek was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum SeekRefused {
    /// The nest named no length for this resource (a proxied upstream that sent
    /// none), so it reads front to back: only the current position is a target.
    UnknownLength,
    /// The target lies before the start.
    BeforeStart,
}

/// One read position over a ranged resource, with a read-ahead window. See the
/// module docs.
#[derive(Debug)]
pub struct RangedCursor {
    len: Option<u64>,
    position: u64,
    window_start: u64,
    window: Vec<u8>,
}

impl RangedCursor {
    /// A cursor at the start of a resource of `len` bytes (`None`: unknown).
    pub fn new(len: Option<u64>) -> Self {
        Self {
            len,
            position: 0,
            window_start: 0,
            window: Vec::new(),
        }
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    /// Seeking needs a known end: a resource of unknown length reads front to
    /// back.
    pub fn can_seek(&self) -> bool {
        self.len.is_some()
    }

    /// Move the cursor. Past the end is allowed (a read there answers nothing);
    /// on a resource of unknown length only the current position is.
    pub fn seek(&mut self, to: SeekFrom) -> Result<u64, SeekRefused> {
        let target = match (to, self.len) {
            (SeekFrom::Start(n), _) => i128::from(n),
            (SeekFrom::Current(d), _) => i128::from(self.position) + i128::from(d),
            (SeekFrom::End(d), Some(len)) => i128::from(len) + i128::from(d),
            (SeekFrom::End(_), None) => return Err(SeekRefused::UnknownLength),
        };
        let target = u64::try_from(target).map_err(|_| SeekRefused::BeforeStart)?;
        if target != self.position && self.len.is_none() {
            return Err(SeekRefused::UnknownLength);
        }
        self.position = target;
        Ok(target)
    }

    /// Fill `out` from the position, which advances by what was read. Fewer
    /// bytes only when the resource ends first; none at or past a known end,
    /// without a request. Bytes come from the window when it covers the
    /// position, else from `fetch(offset, max_len)` — one ranged read of at
    /// least [`READ_AHEAD`] — and an answer narrower than asked is read on from
    /// where it stopped, so a "fill this buffer" read is never mistaken for the
    /// end of the video. An empty answer is the end.
    pub fn read<E>(
        &mut self,
        out: &mut [u8],
        mut fetch: impl FnMut(u64, u64) -> Result<Vec<u8>, E>,
    ) -> Result<usize, E> {
        let room = out.len() as u64;
        let want = match self.len {
            Some(len) => room.min(len.saturating_sub(self.position)),
            None => room,
        };
        let mut filled: u64 = 0;
        while filled < want {
            if !self.window_covers(self.position) {
                let mut ask = (want - filled).max(READ_AHEAD);
                if let Some(len) = self.len {
                    ask = ask.min(len - self.position);
                }
                let chunk = fetch(self.position, ask)?;
                if chunk.is_empty() {
                    break;
                }
                self.window_start = self.position;
                self.window = chunk;
            }
            let at = usize::try_from(self.position - self.window_start).unwrap_or(usize::MAX);
            let take =
                (self.window.len() - at).min(usize::try_from(want - filled).unwrap_or(usize::MAX));
            let dst = usize::try_from(filled).unwrap_or(usize::MAX);
            out[dst..dst + take].copy_from_slice(&self.window[at..at + take]);
            filled += take as u64;
            self.position += take as u64;
        }
        Ok(usize::try_from(filled).unwrap_or(usize::MAX))
    }

    fn window_covers(&self, offset: u64) -> bool {
        offset >= self.window_start && offset < self.window_start + self.window.len() as u64
    }
}

mod imp {
    use super::*;

    pub struct Inner {
        pub reader: Arc<RangedNestReader>,
        pub rt: tokio::runtime::Handle,
        pub cursor: Mutex<RangedCursor>,
    }

    #[derive(Default)]
    pub struct RangedNestStream {
        pub inner: OnceLock<Inner>,
    }

    impl RangedNestStream {
        fn inner(&self) -> Result<&Inner, glib::Error> {
            self.inner.get().ok_or_else(|| {
                glib::Error::new(
                    gio::IOErrorEnum::NotInitialized,
                    "ranged nest stream not opened",
                )
            })
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RangedNestStream {
        const NAME: &'static str = "FaunaRangedNestStream";
        type Type = super::RangedNestStream;
        type ParentType = gio::InputStream;
        type Interfaces = (gio::Seekable,);
    }

    impl ObjectImpl for RangedNestStream {}

    impl InputStreamImpl for RangedNestStream {
        fn read(
            &self,
            buffer: &mut [u8],
            _cancellable: Option<&gio::Cancellable>,
        ) -> Result<usize, glib::Error> {
            let inner = self.inner()?;
            let mut cursor = inner.cursor.lock().unwrap_or_else(|p| p.into_inner());
            cursor
                .read(buffer, |offset, max_len| {
                    inner.rt.block_on(inner.reader.read_at(offset, max_len))
                })
                .map_err(read_error)
        }

        fn close(&self, _cancellable: Option<&gio::Cancellable>) -> Result<(), glib::Error> {
            Ok(())
        }
    }

    impl SeekableImpl for RangedNestStream {
        fn tell(&self) -> i64 {
            self.inner().map_or(0, |inner| {
                let cursor = inner.cursor.lock().unwrap_or_else(|p| p.into_inner());
                i64::try_from(cursor.position()).unwrap_or(i64::MAX)
            })
        }

        fn can_seek(&self) -> bool {
            self.inner().is_ok_and(|inner| {
                inner
                    .cursor
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .can_seek()
            })
        }

        fn seek(
            &self,
            offset: i64,
            type_: glib::SeekType,
            _cancellable: Option<&gio::Cancellable>,
        ) -> Result<(), glib::Error> {
            let inner = self.inner()?;
            let to = match type_ {
                glib::SeekType::Set => SeekFrom::Start(u64::try_from(offset).map_err(|_| {
                    glib::Error::new(gio::IOErrorEnum::InvalidArgument, "seek before the start")
                })?),
                glib::SeekType::End => SeekFrom::End(offset),
                _ => SeekFrom::Current(offset),
            };
            let mut cursor = inner.cursor.lock().unwrap_or_else(|p| p.into_inner());
            cursor
                .seek(to)
                .map(|_| ())
                .map_err(|refused| match refused {
                    SeekRefused::UnknownLength => glib::Error::new(
                        gio::IOErrorEnum::NotSupported,
                        "the nest named no length for this resource, so it reads front to back",
                    ),
                    SeekRefused::BeforeStart => {
                        glib::Error::new(gio::IOErrorEnum::InvalidArgument, "seek before the start")
                    }
                })
        }

        fn can_truncate(&self) -> bool {
            false
        }

        fn truncate(
            &self,
            _offset: i64,
            _cancellable: Option<&gio::Cancellable>,
        ) -> Result<(), glib::Error> {
            Err(glib::Error::new(
                gio::IOErrorEnum::NotSupported,
                "a ranged nest stream is read-only",
            ))
        }
    }

    fn read_error(e: RangedReadError) -> glib::Error {
        glib::Error::new(gio::IOErrorEnum::Failed, &e.to_string())
    }
}

glib::wrapper! {
    /// The seekable `gio::InputStream` `gtk::MediaFile::for_input_stream` plays
    /// a `Url` source from. See the module docs.
    pub struct RangedNestStream(ObjectSubclass<imp::RangedNestStream>)
        @extends gio::InputStream,
        @implements gio::Seekable;
}

impl RangedNestStream {
    /// A stream at the start of `reader`'s resource; `rt` drives its reads.
    pub fn new(reader: Arc<RangedNestReader>, rt: tokio::runtime::Handle) -> Self {
        let stream: Self = glib::Object::new();
        let len = reader.len();
        let _ = stream.imp().inner.set(imp::Inner {
            reader,
            rt,
            cursor: Mutex::new(RangedCursor::new(len)),
        });
        stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource of `len` bytes whose byte `i` is `i as u8`, counting the
    /// ranged reads asked of it. `cap` narrows every answer, as a server
    /// answering less than the asked range would.
    struct Resource {
        len: u64,
        cap: u64,
        asks: Vec<(u64, u64)>,
    }

    impl Resource {
        fn new(len: u64) -> Self {
            Self {
                len,
                cap: u64::MAX,
                asks: Vec::new(),
            }
        }

        fn fetch(&mut self, offset: u64, max_len: u64) -> Result<Vec<u8>, ()> {
            self.asks.push((offset, max_len));
            let end = self.len.min(offset.saturating_add(max_len.min(self.cap)));
            Ok((offset..end).map(|i| i as u8).collect())
        }
    }

    fn read(cursor: &mut RangedCursor, res: &mut Resource, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let got = cursor.read(&mut buf, |o, m| res.fetch(o, m)).unwrap();
        buf.truncate(got);
        buf
    }

    /// GStreamer's 4 KiB block reads are served from one read-ahead window, so
    /// a clip costs a GET per window, never one per block.
    #[test]
    fn small_reads_share_one_read_ahead_window() {
        let mut res = Resource::new(1 << 20);
        let mut cursor = RangedCursor::new(Some(1 << 20));
        for block in 0..64u64 {
            let bytes = read(&mut cursor, &mut res, 4096);
            assert_eq!(bytes.len(), 4096);
            assert_eq!(bytes[0], (block * 4096) as u8);
        }
        assert_eq!(res.asks, vec![(0, READ_AHEAD)]);
        assert_eq!(cursor.position(), 64 * 4096);
    }

    /// A known end clamps the read and the ask; at the end a read answers
    /// nothing, without a request.
    #[test]
    fn a_known_length_clamps_and_ends_without_a_request() {
        let mut res = Resource::new(10);
        let mut cursor = RangedCursor::new(Some(10));
        assert_eq!(
            read(&mut cursor, &mut res, 4096),
            (0..10).collect::<Vec<u8>>()
        );
        assert_eq!(res.asks, vec![(0, 10)]);
        assert!(read(&mut cursor, &mut res, 4096).is_empty());
        assert_eq!(res.asks.len(), 1, "a read at the known end asks nothing");
    }

    /// A server answering narrower than asked is read on from where it
    /// stopped: the buffer fills, and the short answer is not the end.
    #[test]
    fn a_short_answer_is_read_on() {
        let mut res = Resource::new(1000);
        res.cap = 100;
        let mut cursor = RangedCursor::new(Some(1000));
        let bytes = read(&mut cursor, &mut res, 1000);
        assert_eq!(bytes.len(), 1000);
        assert_eq!(bytes[999], (999u32 % 256) as u8);
        assert_eq!(res.asks.len(), 10);
    }

    /// A seek outside the window moves the next GET there; one inside it costs
    /// nothing.
    #[test]
    fn seeks_move_the_read_and_reuse_the_window() {
        let len = 4 * READ_AHEAD;
        let mut res = Resource::new(len);
        let mut cursor = RangedCursor::new(Some(len));
        read(&mut cursor, &mut res, 16);
        cursor.seek(SeekFrom::Start(100)).unwrap();
        assert_eq!(read(&mut cursor, &mut res, 1), vec![100]);
        assert_eq!(res.asks.len(), 1, "a seek inside the window re-reads it");

        cursor.seek(SeekFrom::End(-1)).unwrap();
        assert_eq!(read(&mut cursor, &mut res, 8), vec![(len - 1) as u8]);
        assert_eq!(res.asks[1], (len - 1, 1));
        assert_eq!(
            cursor.seek(SeekFrom::Current(-(len as i64) - 1)),
            Err(SeekRefused::BeforeStart)
        );
    }

    /// A resource of unknown length reads front to back to its empty answer,
    /// and refuses every seek but to where it stands.
    #[test]
    fn an_unknown_length_reads_front_to_back() {
        let mut res = Resource::new(5000);
        let mut cursor = RangedCursor::new(None);
        assert!(!cursor.can_seek());
        assert_eq!(cursor.seek(SeekFrom::Start(0)), Ok(0));
        assert_eq!(
            cursor.seek(SeekFrom::Start(9)),
            Err(SeekRefused::UnknownLength)
        );
        assert_eq!(
            cursor.seek(SeekFrom::End(0)),
            Err(SeekRefused::UnknownLength)
        );
        assert_eq!(read(&mut cursor, &mut res, 8192).len(), 5000);
        assert!(read(&mut cursor, &mut res, 8192).is_empty());
    }

    /// A failed ranged read is the read's error — the player surfaces it.
    #[test]
    fn a_failed_fetch_fails_the_read() {
        let mut cursor = RangedCursor::new(Some(10));
        let mut buf = [0u8; 4];
        assert_eq!(
            cursor.read(&mut buf, |_, _| Err::<Vec<u8>, _>("refused")),
            Err("refused")
        );
        assert_eq!(cursor.position(), 0);
    }
}
