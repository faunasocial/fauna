//! Semaphore-bounded chunk transfer pool.
//!
//! Provides [`upload_chunks`] and [`download_chunks`] that run transfers
//! concurrently (bounded by [`AdaptiveConcurrency`]), report progress,
//! and feed back success/error to the adaptive controller.
//!
//! This module does NOT interact with the database — callers are responsible
//! for queue management based on the returned results.

use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_core::data::ContentHash;

use crate::adaptive::AdaptiveConcurrency;
use crate::nest_client::SyncClient;
use crate::progress::{self, ProgressEvent, ProgressTx};

/// Result of a single chunk upload attempt.
pub struct UploadResult {
    pub hash: ContentHash,
    pub success: bool,
    pub bytes: usize,
}

/// Shared transfer pool state.
pub struct TransferPool {
    pub concurrency: Arc<AdaptiveConcurrency>,
    pub progress_tx: ProgressTx,
}

impl TransferPool {
    pub fn new(concurrency: Arc<AdaptiveConcurrency>, progress_tx: ProgressTx) -> Self {
        Self {
            concurrency,
            progress_tx,
        }
    }

    /// Upload a set of chunks concurrently.
    ///
    /// Returns an `UploadResult` per chunk indicating success/failure.
    /// The caller is responsible for updating the transfer queue based
    /// on these results.
    pub async fn upload_chunks(
        &self,
        client: &SyncClient,
        chunks: &[(ContentHash, Vec<u8>)],
        path: &str,
    ) -> Vec<UploadResult> {
        use futures_util::stream::{self, StreamExt};
        use std::sync::Mutex;

        let results = Mutex::new(Vec::with_capacity(chunks.len()));

        stream::iter(chunks.iter())
            .for_each_concurrent(None, |(hash, data)| {
                let results = &results;
                async move {
                    let _permit = self.concurrency.acquire().await;
                    let outcome = client.upload_chunk(hash, data).await;
                    let success = outcome.is_ok();
                    if success {
                        self.concurrency.record_success();
                        progress::emit(
                            &self.progress_tx,
                            ProgressEvent::ChunkDone {
                                path: path.to_string(),
                                bytes: data.len() as u64,
                            },
                        );
                        tracing::debug!(
                            chunk = hex::encode(hash.digest()),
                            size = data.len(),
                            "uploaded chunk"
                        );
                    } else {
                        self.concurrency.record_error();
                        if let Err(ref e) = outcome {
                            tracing::warn!(
                                chunk = hex::encode(hash.digest()),
                                error = %e,
                                "chunk upload failed"
                            );
                        }
                    }
                    results.lock().unwrap().push(UploadResult {
                        hash: *hash,
                        success,
                        bytes: data.len(),
                    });
                }
            })
            .await;

        results.into_inner().unwrap()
    }

    /// Download chunks concurrently, returning them in the original order.
    ///
    /// Each chunk is downloaded with semaphore-bounded concurrency. The
    /// results are collected into a Vec in the same order as `hashes`.
    /// The `buffer_unordered` limit is set high — the semaphore is the
    /// real concurrency bound.
    pub async fn download_chunks(
        &self,
        client: &SyncClient,
        hashes: &[ContentHash],
        path: &str,
    ) -> Result<Vec<Vec<u8>>> {
        use futures_util::stream::{self, StreamExt};

        let max_buf = self.concurrency.current().max(8) as usize;

        // Iterate OWNED hashes (a 32-byte content address — the clone is free at
        // this scale). A borrowing `hashes.iter()` gives the closure a lifetime in
        // its signature, which fails the higher-ranked bound the moment this call
        // sits inside an `async_trait`-boxed future — as it now does, via
        // `EngineBlobFetcher`'s binding of the shared `BlobFetcher` seam.
        let results: Vec<Result<(usize, Vec<u8>)>> =
            stream::iter(hashes.iter().cloned().enumerate())
                .map(|(idx, hash)| async move {
                    let _permit = self.concurrency.acquire().await;
                    let result = client.download_chunk(&hash).await;
                    match &result {
                        Ok(data) => {
                            self.concurrency.record_success();
                            progress::emit(
                                &self.progress_tx,
                                ProgressEvent::ChunkDone {
                                    path: path.to_string(),
                                    bytes: data.len() as u64,
                                },
                            );
                        }
                        Err(_) => {
                            self.concurrency.record_error();
                        }
                    }
                    result.map(|data| (idx, data)).with_context(|| {
                        format!("downloading chunk {}", hex::encode(hash.digest()))
                    })
                })
                .buffer_unordered(max_buf)
                .collect()
                .await;

        // Sort by original index and extract data
        let mut indexed: Vec<(usize, Vec<u8>)> = Vec::with_capacity(hashes.len());
        for r in results {
            indexed.push(r?);
        }
        indexed.sort_by_key(|(idx, _)| *idx);
        Ok(indexed.into_iter().map(|(_, data)| data).collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::adaptive::AdaptiveConcurrency;
    use crate::progress::ProgressEvent;

    #[tokio::test]
    async fn transfer_pool_reports_progress() {
        let ac = Arc::new(AdaptiveConcurrency::fixed(4));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
        let pool = TransferPool::new(ac, Some(tx));

        // Emit a chunk-done event directly to test the plumbing
        crate::progress::emit(
            &pool.progress_tx,
            ProgressEvent::ChunkDone {
                path: "test.txt".into(),
                bytes: 1024,
            },
        );

        let event = rx.try_recv().unwrap();
        assert!(matches!(
            event,
            ProgressEvent::ChunkDone { bytes: 1024, .. }
        ));
    }

    #[tokio::test]
    async fn transfer_pool_no_progress_when_none() {
        let ac = Arc::new(AdaptiveConcurrency::fixed(4));
        let pool = TransferPool::new(ac, None);

        // Should not panic with None sender
        crate::progress::emit(
            &pool.progress_tx,
            ProgressEvent::ChunkDone {
                path: "test.txt".into(),
                bytes: 512,
            },
        );
    }
}
