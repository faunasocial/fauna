//! The peer leg's **admitted-sibling registry** — the one
//! [`SiblingChunkSource`] (`docs/goal/behavior/file-sync.md` § Content
//! residency: seats fetch content seat↔seat over the account plane's peer
//! leg, "the same want-list chunk pull").
//!
//! The dial pass (`crate::peer_leg::dial_pass`) records every sibling it
//! admitted — mutually, over the same-account `DeviceAuthorization` witness —
//! and keeps its channel open here until the next pass replaces it. A download
//! in any engine of this host then asks those channels for its chunk bodies
//! first (`fauna_peer_sync::pull_file_chunks`), and the nest serves the rest.
//!
//! **What a sibling serves never decides the transfer.** A body is kept only
//! once it hashes to its store key; a sibling that errs — a dropped
//! connection, a refused slice, a forged body — is dropped from the registry
//! and the next one (or the nest) asked; a relayed connection while the nest
//! answers moves no bytes (`p2p.md` § The relay, ruling 4). So the source
//! contract — infallible, keyed by what arrived — holds by construction.
//!
//! Severance stays the serve side's: the channel was admitted under this
//! side's views, and the sibling's own server re-checks removal and expiry on
//! every request, so a device removed since the pass is refused at its next
//! chunk pull, and its channel dropped here with the error.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fauna_core::data::ContentHash;
use fauna_peer_channel::PeerChannel;
use fauna_transport::NestPath;

use crate::sibling_source::SiblingChunkSource;

/// What this host's downloads moved over the peer leg since start — the
/// observable that says whether a body came from a sibling or the nest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SiblingTally {
    /// Chunk bodies a sibling served (verified on completion).
    pub chunks: u64,
    /// The bytes of those bodies.
    pub bytes: u64,
    /// Chunks wanted from a sibling and left to the nest: missing there,
    /// relay-deferred, or lost to a sibling that erred.
    pub to_nest: u64,
}

/// See the module docs.
#[derive(Default)]
pub struct SiblingChannels {
    inner: Mutex<Inner>,
    chunks: AtomicU64,
    bytes: AtomicU64,
    to_nest: AtomicU64,
}

/// Admitted siblings by NodeId, in admission order.
type Admitted = Vec<([u8; 32], Arc<PeerChannel>)>;

#[derive(Default)]
struct Inner {
    channels: Admitted,
    /// Whether the last pass's nest walk answered — the byte gate's nest fact.
    /// `None` before the first pass, read as reachable: the answer that keeps
    /// a relayed connection's bytes on the nest path.
    nest: Option<NestPath>,
}

impl SiblingChannels {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Hold `channel` as `node_id`'s admitted channel, replacing an older one.
    pub(crate) fn admitted(&self, node_id: [u8; 32], channel: Arc<PeerChannel>) {
        let mut inner = self.inner.lock().expect("sibling registry");
        inner.channels.retain(|(id, _)| *id != node_id);
        inner.channels.push((node_id, channel));
    }

    /// Forget `node_id` — it failed this pass, or failed a pull.
    pub(crate) fn dropped(&self, node_id: &[u8; 32]) {
        self.inner
            .lock()
            .expect("sibling registry")
            .channels
            .retain(|(id, _)| id != node_id);
    }

    /// Forget every sibling — the leg went down.
    pub(crate) fn clear(&self) {
        self.inner
            .lock()
            .expect("sibling registry")
            .channels
            .clear();
    }

    /// Record the pass's nest fact for the byte gate.
    pub(crate) fn set_nest(&self, nest: NestPath) {
        self.inner.lock().expect("sibling registry").nest = Some(nest);
    }

    /// How many siblings are held right now.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("sibling registry").channels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// What downloads moved over the peer leg since start.
    pub fn tally(&self) -> SiblingTally {
        SiblingTally {
            chunks: self.chunks.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            to_nest: self.to_nest.load(Ordering::Relaxed),
        }
    }

    fn snapshot(&self) -> (Admitted, NestPath) {
        let inner = self.inner.lock().expect("sibling registry");
        (
            inner.channels.clone(),
            inner.nest.unwrap_or(NestPath::Reachable),
        )
    }
}

#[async_trait::async_trait]
impl SiblingChunkSource for SiblingChannels {
    async fn fetch(
        &self,
        folder: &str,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> HashMap<ContentHash, Vec<u8>> {
        let mut got: HashMap<ContentHash, Vec<u8>> = HashMap::new();
        let (siblings, nest) = self.snapshot();
        if siblings.is_empty() || store_keys.is_empty() {
            return got;
        }
        let mut wanted: Vec<ContentHash> = Vec::new();
        for key in store_keys {
            if !wanted.contains(key) {
                wanted.push(*key);
            }
        }
        let asked = wanted.len() as u64;
        for (node_id, channel) in siblings {
            if wanted.is_empty() {
                break;
            }
            match fauna_peer_sync::pull_file_chunks(&channel, folder, &wanted, relative_path, nest)
                .await
            {
                Ok(pull) => {
                    wanted.retain(|k| !pull.bodies.contains_key(k));
                    got.extend(pull.bodies);
                }
                Err(e) => {
                    tracing::debug!(
                        node = %fauna_core::hex32::encode(&node_id),
                        path = %fauna_core::log_redact::log_path(relative_path),
                        "sibling chunk pull failed; the nest path serves the rest: {e:#}"
                    );
                    self.dropped(&node_id);
                }
            }
        }
        let bytes: u64 = got.values().map(|b| b.len() as u64).sum();
        self.chunks.fetch_add(got.len() as u64, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        self.to_nest
            .fetch_add(asked - got.len() as u64, Ordering::Relaxed);
        if !got.is_empty() {
            tracing::debug!(
                path = %fauna_core::log_redact::log_path(relative_path),
                from_sibling = got.len(),
                to_nest = asked - got.len() as u64,
                "chunk bodies fetched from a sibling device"
            );
        }
        got
    }
}
