//! **The MDA bridge's session-scoped mail/calendar index handle** — the FFI leg
//! of `docs/goal/behavior/content-index.md` § Where the index is built.
//!
//! **Query-only since the 2026-08-10 carrier ruling** (that section owns it):
//! the MDA's *build* half is retired — it never held the mail kind's ratified
//! content id (the RFC `Message-ID`), so every doc it staged duplicated a
//! client-built one under a spelling no other leg could see. The slice is built
//! by the client leg (`fauna-client-index`); this session **reads** it, deciding
//! `SEARCH` coverage against each doc's stored *secondary* identity — the raw
//! nest message id, which is exactly the spelling this caller holds.
//!
//! The Go MDA holds a MUA session's MSEK inside an [`MlsCapability`] and may
//! therefore query **mail and calendar only** — never the cross-kind index
//! master key (`key-material-hierarchy.md` rule #7, the blast-radius
//! invariant). This module is how it reaches that slice without the key
//! material ever crossing the boundary.
//!
//! # Why the seam is shaped like this
//!
//! Two constraints decide the shape, and neither is negotiable:
//!
//! 1. **The MSEK never crosses the FFI boundary.** [`MlsCapability`] keeps it a
//!    private `Zeroizing<[u8; 32]>` and hands out opaque handles; so the session
//!    is *constructed from* the capability (which derives the key internally)
//!    rather than from bytes the caller could hold. Same rule, same shape as
//!    `MlsCapability::new_epoch_aware_mail_record_opener`.
//! 2. **The transport is on the Go side.** The `__index` rail read is
//!    `fauna.bridges.index_list` over the MDA's own WS-RPC connection plus the
//!    blob GET route — a connection this crate does not have and must not open
//!    a second copy of. So the rail is a **foreign trait** ([`FfiIndexRail`])
//!    the caller implements, read-only by construction now that nothing here
//!    publishes.
//!
//! The rail's methods are deliberately **synchronous**. `SegmentRail` is async
//! because the client leg's implementation is; a foreign implementation is a
//! blocking call into the host language either way, and a sync foreign trait is
//! the shape every binding generator supports. The adapter below bridges the
//! two on a current-thread runtime — there is no I/O in Rust here to need a
//! reactor.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use fauna_client_index::{
    IndexBuildError, MailIndexReader, MailcalKeyRing, RailEntry, SegmentRail, open_mail_reader,
};
use fauna_index::{ContentKind, QueryHit};

use crate::FfiError;
use crate::mail::MlsCapability;

/// One `__index` rail entry as the foreign side reports it — the FFI mirror of
/// `fauna_client_index::RailEntry`.
#[derive(uniffi::Record, Debug, Clone)]
pub struct FfiIndexRailEntry {
    /// The `__index`-relative virtual path the blob was published at.
    pub path: String,
    /// Hex blake3 of the sealed blob — the handle [`FfiIndexRail::fetch_blob`]
    /// takes.
    pub blob_hash: String,
    /// Sealed size in bytes. Drives the fold planner's budget, so a caller that
    /// cannot report it should report the real byte length rather than 0.
    pub size_bytes: u64,
}

/// The `__index` rail's **read half**, implemented by the caller's transport.
///
/// The MDA implements this over `fauna.bridges.index_list` (which names the
/// target actor explicitly and is `BridgeMda`-only) plus the blob GET route.
/// **Everything that crosses it is already AEAD-sealed** under a key no nest
/// holds, so an implementation needs no key material and can never be asked
/// for any.
///
/// Read-only by construction (the 2026-08-10 carrier ruling retired the MDA's
/// build half — see the module docs), so there is no publish method to
/// implement and no ordering contract to honour.
///
/// Synchronous on purpose (see the module docs). An implementation may block —
/// it is called from a dedicated runtime that has nothing else to progress.
#[uniffi::export(with_foreign)]
pub trait FfiIndexRail: Send + Sync {
    /// Everything the rail currently lists for this actor, in one call.
    fn list_entries(&self) -> Result<Vec<FfiIndexRailEntry>, FfiError>;

    /// Sealed bytes for a blob hash from [`Self::list_entries`].
    fn fetch_blob(&self, blob_hash: String) -> Result<Vec<u8>, FfiError>;
}

/// Adapts the caller's synchronous [`FfiIndexRail`] to the async
/// [`SegmentRail`] the shared builder is written against.
///
/// Nothing here awaits: each method calls straight through and returns a ready
/// future. That is what makes it correct to drive the builder on a
/// current-thread runtime with no I/O driver.
struct ForeignRail(Arc<dyn FfiIndexRail>);

impl ForeignRail {
    /// A foreign failure is a rail failure, not an index-format failure — it
    /// maps to the variant the builder already treats as retryable, so a
    /// transient nest outage costs the session its index and nothing else.
    fn publish_err(path: &str, e: FfiError) -> IndexBuildError {
        IndexBuildError::Publish {
            path: path.to_string(),
            reason: e.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl SegmentRail for ForeignRail {
    async fn list_entries(&self) -> Result<Vec<RailEntry>, IndexBuildError> {
        Ok(self
            .0
            .list_entries()
            .map_err(|e| Self::publish_err("__index", e))?
            .into_iter()
            .map(|e| RailEntry {
                path: e.path,
                blob_hash: e.blob_hash,
                size_bytes: e.size_bytes,
            })
            .collect())
    }

    async fn fetch_blob(&self, blob_hash: &str) -> Result<Vec<u8>, IndexBuildError> {
        self.0
            .fetch_blob(blob_hash.to_string())
            .map_err(|e| Self::publish_err(&format!("blob/{blob_hash}"), e))
    }

    async fn publish(&self, path: &str, _bytes: &[u8]) -> Result<(), IndexBuildError> {
        // Unreachable by construction: this session never constructs a builder,
        // and the read paths (`open_mail_reader`) only list and fetch. A call
        // landing here is a bug, and an error beats a silent write into a rail
        // the foreign side no longer implements a publish for.
        Err(Self::publish_err(
            path,
            FfiError::General {
                msg: "the MDA index session is query-only — nothing may publish through it \
                      (content-index.md § Where the index is built, 2026-08-10 ruling)"
                    .into(),
            },
        ))
    }
}

/// What the index could say about one `SEARCH`'s candidate messages —
/// [`FfiMailIndexSession::answer_body_search`]'s reply.
///
/// Two sets rather than one, because "did not match" and "cannot say" are
/// different answers and the caller must treat them differently: a candidate
/// **absent from `covered`** still needs its hint opened and scanned, while one
/// present in `covered` but absent from `matched` has been answered — *no* —
/// and must not be scanned again.
#[derive(uniffi::Record, Debug, Clone)]
pub struct FfiIndexAnswer {
    /// The candidates the published slice can answer for.
    pub covered: Vec<String>,
    /// The subset of [`Self::covered`] matching the search terms. Always a
    /// subset — never a message the caller did not offer.
    pub matched: Vec<String>,
}

/// A MUA session's handle on the user's mail/calendar index slice.
///
/// Lifecycle mirrors the credential it was derived from
/// (`imap-server.md` § Authentication): construct at AUTH from the session's
/// [`MlsCapability`], drive it while the session runs, [`zeroize`] it at LOGOUT
/// / idle timeout / disconnect. After [`zeroize`] every method fails — the same
/// contract as the capability itself.
///
/// [`zeroize`]: Self::zeroize
#[derive(uniffi::Object)]
pub struct FfiMailIndexSession {
    inner: Mutex<Option<SessionInner>>,
    /// Drives the shared builder's async surface. Current-thread and
    /// I/O-driverless on purpose: every await in this path resolves
    /// immediately, because the only I/O is the caller's blocking rail.
    rt: tokio::runtime::Runtime,
}

struct SessionInner {
    /// Reads span MSEK rotations (see [`MailcalKeyRing`]); this session has no
    /// write half to need the current generation for.
    ring: MailcalKeyRing,
    rail: Arc<ForeignRail>,
    /// The opened slice, kept between queries so an IMAP `SEARCH` does not
    /// refetch and re-unseal every segment per command. Invalidated by the
    /// manifest hash moving, which is exactly the set of events that change
    /// what a query can find.
    reader: Option<MailIndexReader>,
}

impl std::fmt::Debug for FfiMailIndexSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiMailIndexSession")
            .field("zeroized", &self.inner.lock().map_or(true, |g| g.is_none()))
            .finish()
    }
}

/// The kinds a mail/calendar index-segment key opens. A query is intersected
/// with this by the shared reader, so asking for more can only ever narrow.
const SESSION_KINDS: &[ContentKind] = &[ContentKind::Mail, ContentKind::Calendar];

#[uniffi::export]
impl MlsCapability {
    /// Open this session's mail/calendar index slice for querying.
    ///
    /// The one call the MDA makes at AUTH. Query-only (the 2026-08-10 carrier
    /// ruling — module docs): the slice is opened lazily at the first query, so
    /// this costs nothing on the rail at AUTH and a rail outage surfaces as a
    /// per-query degradation, not an AUTH failure.
    ///
    /// `snapshot_plaintext_bytes` is the plaintext of a PRIOR
    /// `self.decrypt(snapshot_blob_bytes)` on this same capability. Its
    /// `index_seg_grace_keys` are what let this session open segments sealed
    /// before the last MSEK rotation; passing a snapshot without them simply
    /// yields a current-generation-only reach.
    ///
    /// # Errors
    ///
    /// `FfiError::General` if this capability is zeroized or the snapshot does
    /// not decode. **A later rail failure is recoverable and the caller should
    /// treat it so** — serve `SEARCH` the old way for that command, exactly as
    /// the client leg skips a launch rather than failing login.
    pub fn resume_mail_index_session(
        &self,
        snapshot_plaintext_bytes: Vec<u8>,
        rail: Arc<dyn FfiIndexRail>,
    ) -> Result<Arc<FfiMailIndexSession>, FfiError> {
        let msek = self.msek_copy()?;
        let ring = MailcalKeyRing::from_msek_and_snapshot(&msek, &snapshot_plaintext_bytes)
            .map_err(|e| FfiError::General {
                msg: format!("mail-index session: {e}"),
            })?;
        let rail = Arc::new(ForeignRail(rail));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|e| FfiError::General {
                msg: format!("mail-index session runtime: {e}"),
            })?;
        Ok(Arc::new(FfiMailIndexSession {
            inner: Mutex::new(Some(SessionInner {
                ring,
                rail,
                reader: None,
            })),
            rt,
        }))
    }
}

#[uniffi::export]
impl FfiMailIndexSession {
    /// Search this session's mail/calendar slice.
    ///
    /// This is what replaces the MDA's linear scan over per-message sealed index
    /// hints as IMAP `SEARCH`'s body axis. The answer is confined to mail and
    /// calendar by the key class — asking for other kinds cannot widen it.
    ///
    /// Re-opens the slice only when the published manifest has moved since the
    /// last query, so a `SEARCH`-heavy session pays one cheap rail listing per
    /// command rather than a full refetch.
    pub fn query(&self, query: String, limit: u32) -> Result<Vec<QueryHit>, FfiError> {
        let mut guard = self.locked()?;
        let inner = guard.as_mut().ok_or_else(zeroized)?;
        let reader = self.refreshed_reader(inner)?;
        reader
            .query(&query, SESSION_KINDS, None, limit as usize)
            .map_err(|e| FfiError::General {
                msg: format!("mail-index query: {e}"),
            })
    }

    /// Answer an IMAP `SEARCH` body axis over the messages this slice covers.
    ///
    /// One call rather than three, because the two halves must be decided
    /// against the **same** opened slice: which of `candidates` this session can
    /// answer for, and which of those match `terms`.
    ///
    /// # Why the caller cannot just query
    ///
    /// The MDA's slice is built opportunistically — it holds what earlier
    /// `SEARCH`es happened to decrypt, never "the mailbox". So an index-only
    /// answer silently under-reports, and the caller must scan whatever the
    /// index cannot speak for. `covered` is that dividing line, and it is the
    /// **queryable** set (the opened reader's live ids), *not* the builder's
    /// re-index guard: the guard also holds ids staged but not yet flushed, and
    /// the MDA's flush is best-effort, so trusting it would let a message whose
    /// segment never reached the rail vanish from `SEARCH` with no error.
    ///
    /// `matched ⊆ covered` by construction. Terms that tokenize to nothing
    /// impose no constraint, so every covered candidate matches — the same
    /// answer the linear hint scan gives, which is what keeps the two paths in
    /// parity when a user searches for punctuation.
    ///
    /// Candidates the slice does not cover are simply absent from `covered`;
    /// that is a normal state (a fresh session covers nothing at all), not an
    /// error.
    ///
    /// **`candidates` are lowercase hex of the nest's binary message id** — the
    /// one identity this caller holds — and coverage is decided against each
    /// doc's stored *secondary* identity, which the client leg stamps from the
    /// same nest id at ingest (`content-index.md` § Where the index is built →
    /// the 2026-08-10 carrier ruling). The content id stays the RFC
    /// `Message-ID` and is never compared against a candidate: the two
    /// spellings are disjoint for every message, which is exactly the
    /// inertness this carrier exists to end. A doc with no secondary identity
    /// (no `nest_message_id` at ingest) is simply not covered — the scan arm answers
    /// for it.
    pub fn answer_body_search(
        &self,
        terms: Vec<String>,
        candidates: Vec<String>,
    ) -> Result<FfiIndexAnswer, FfiError> {
        let mut guard = self.locked()?;
        let inner = guard.as_mut().ok_or_else(zeroized)?;
        let reader = self.refreshed_reader(inner)?;

        // One walk serves both directions: the coverage set (secondary ids
        // live in the slice) and the translation map (content id → secondary)
        // that turns matched hits back into the caller's spelling.
        let identities = reader.doc_identities().map_err(|e| FfiError::General {
            msg: format!("mail-index coverage: {e}"),
        })?;
        let live_secondary: HashSet<&[u8]> = identities
            .iter()
            .filter_map(|d| d.secondary_id.as_deref())
            .collect();
        let secondary_of_content: HashMap<&[u8], &[u8]> = identities
            .iter()
            .filter_map(|d| {
                d.secondary_id
                    .as_deref()
                    .map(|s| (d.content_id.0.as_slice(), s))
            })
            .collect();

        let mut covered: Vec<String> = Vec::new();
        let mut covered_bytes: HashSet<Vec<u8>> = HashSet::new();
        for c in candidates {
            let Ok(bytes) = decode_hex(&c) else {
                // Not hex ⇒ not a nest message id ⇒ nothing this slice could
                // ever cover. Absent from `covered` is the honest answer.
                continue;
            };
            if live_secondary.contains(bytes.as_slice()) {
                covered_bytes.insert(bytes);
                covered.push(c);
            }
        }

        let tokens = fauna_index::query_tokens(&terms);
        let matched: Vec<String> = if covered.is_empty() {
            Vec::new()
        } else if tokens.is_empty() {
            // No constraint — the scan would return every message it saw, so
            // the covered half of the same mailbox is exactly this path's share.
            covered.clone()
        } else {
            // Unbounded: a `SEARCH` reply is a complete set, never a top-N page.
            let hits = reader
                .query_all_of(&tokens, SESSION_KINDS, None, None)
                .map_err(|e| FfiError::General {
                    msg: format!("mail-index query: {e}"),
                })?;
            let hit_secondary: HashSet<&[u8]> = hits
                .iter()
                .filter_map(|h| secondary_of_content.get(h.content_id.0.as_slice()).copied())
                .collect();
            covered
                .iter()
                .filter(|c| {
                    decode_hex(c).is_ok_and(|bytes| hit_secondary.contains(bytes.as_slice()))
                })
                .cloned()
                .collect()
        };
        Ok(FfiIndexAnswer { covered, matched })
    }

    /// Drop the derived key material and the opened slice now. Idempotent — a
    /// second call is a no-op — and every other method fails afterwards, the
    /// same contract as [`MlsCapability::zeroize`].
    pub fn zeroize(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.take();
        }
    }
}

/// Lowercase-or-uppercase hex → bytes; `Err(())` on anything that is not hex.
fn decode_hex(s: &str) -> Result<Vec<u8>, ()> {
    fauna_core::format::hex_decode(s).ok_or(())
}

impl FfiMailIndexSession {
    fn locked(&self) -> Result<std::sync::MutexGuard<'_, Option<SessionInner>>, FfiError> {
        self.inner.lock().map_err(|e| FfiError::General {
            msg: format!("mail-index session lock poisoned: {e}"),
        })
    }

    /// The opened slice, re-opened only when the published manifest has moved.
    ///
    /// Shared by every read entry point so they cannot drift on when a reader is
    /// considered stale — the manifest hash is the authority on what the index
    /// contains, so its moving is exactly the condition that invalidates one.
    ///
    /// Outside the `#[uniffi::export]` block deliberately: it borrows from the
    /// caller's guard, and the export macro cannot carry that lifetime.
    fn refreshed_reader<'a>(
        &self,
        inner: &'a mut SessionInner,
    ) -> Result<&'a MailIndexReader, FfiError> {
        let published = self
            .rt
            .block_on(async {
                let entries = SegmentRail::list_entries(inner.rail.as_ref()).await?;
                Ok::<_, IndexBuildError>(
                    entries
                        .into_iter()
                        .find(|e| e.path == fauna_index::mailcal_manifest_path())
                        .map(|e| e.blob_hash),
                )
            })
            .map_err(|e| FfiError::General {
                msg: format!("mail-index list: {e}"),
            })?;
        let fresh = inner
            .reader
            .as_ref()
            .is_some_and(|r| r.manifest_hash() == published.as_deref());
        if !fresh {
            inner.reader = Some(
                self.rt
                    .block_on(open_mail_reader(&inner.ring, inner.rail.as_ref()))
                    .map_err(|e| FfiError::General {
                        msg: format!("mail-index open: {e}"),
                    })?,
            );
        }
        Ok(inner.reader.as_ref().expect("just opened"))
    }
}

fn zeroized() -> FfiError {
    FfiError::General {
        msg: "mail-index session already zeroized".into(),
    }
}
