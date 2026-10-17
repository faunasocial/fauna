//! UniFFI face of [`fauna_client::RangedNestReader`] — the native-player leg
//! (security.md § Transport trust → *The native-player leg*): windows, macos +
//! ios and android wrap it in their platform's byte-source seam
//! (render-model.md § D6c → *Inline playback*, answer (1)); linux consumes the
//! Rust type directly; web is outside the leg (same-origin).
//!
//! **Sync on purpose.** Every caller is a platform media thread that reads
//! synchronously — `MediaDataSource.readAt`, an `AVAssetResourceLoaderDelegate`
//! queue, the thread behind an `IRandomAccessStream.ReadAsync` — so each call
//! blocks on the reader's own current-thread runtime (the construct-and-block
//! shape of `snapshot_download` and `provisioning`). Concurrent reads from two
//! player threads are fine: a current-thread runtime lets one `block_on` drive
//! while the other waits. Never call it from inside an async executor.

use std::sync::Arc;

use fauna_client::RangedNestReader;
use tokio::runtime::{Builder, Runtime};

use crate::FfiError;

/// A random-access, SPKI-pinned reader over one nest resource.
#[derive(uniffi::Object)]
pub struct FfiRangedNestReader {
    rt: Runtime,
    inner: RangedNestReader,
}

fn reader_err(e: impl std::fmt::Display) -> FfiError {
    FfiError::General { msg: e.to_string() }
}

#[uniffi::export]
impl FfiRangedNestReader {
    /// Probe `path` (nest-relative, its query carried verbatim — e.g. the
    /// playback ticket) on `nest_url` with one ranged GET. Fails when the nest
    /// is unreachable, its cert is neither WebPKI-valid nor the graduated pin,
    /// or it answers anything but 200 / 206 / 416.
    #[uniffi::constructor]
    pub fn open(nest_url: String, path: String) -> Result<Arc<Self>, FfiError> {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| reader_err(format!("ranged reader runtime: {e}")))?;
        let inner = rt
            .block_on(RangedNestReader::open(&nest_url, &path))
            .map_err(reader_err)?;
        Ok(Arc::new(Self { rt, inner }))
    }

    /// The total length, or `None` when the nest named none (report unknown
    /// length to the player).
    // An `is_empty` would be one more exported method no player seam asks for.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> Option<u64> {
        self.inner.len()
    }

    /// The resource's `Content-Type`, if the nest sent one.
    pub fn content_type(&self) -> Option<String> {
        self.inner.content_type().map(str::to_string)
    }

    /// Up to `max_len` bytes at `offset` — shorter at EOF, empty past it. One
    /// ranged GET; blocks the calling thread.
    pub fn read_at(&self, offset: u64, max_len: u64) -> Result<Vec<u8>, FfiError> {
        self.rt
            .block_on(self.inner.read_at(offset, max_len))
            .map_err(reader_err)
    }
}
