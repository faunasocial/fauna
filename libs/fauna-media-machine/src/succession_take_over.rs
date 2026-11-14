//! **The take-over where no engine runs for the set** — `succession-cut.md`
//! ruling (11)(l): on the device that made the succession cut, for a set no
//! sync engine on that device is bound to, the identity app's own projection
//! walk re-records what the cut made history. Every set on web is such a set
//! (web runs no engine), so without this walk a cut made there leaves every
//! pre-ceremony file of every owned set listed for nobody — the owner's Media
//! page and every member's alike — since a Media listing never lists a
//! history row as the item (ruling (11)(c)).
//!
//! **Why here.** It is the served-set walk's twin
//! ([`crate::served_reseal`]): the same seat, the same change-log read, the
//! same verified-row judge, and the signed record through the seat's own
//! writer key — so nothing is re-bound per app, and every app that reads sets
//! with no engine runs one implementation.
//!
//! **What this walk owns, and what it does not.** It answers which heads judge
//! history ([`TakeOverWalk::history_heads`]) and re-signs the heads it is
//! handed ([`TakeOverWalk::take_over`]). The adoption marker — which device
//! may adopt at all, the claim that keeps the engine from adopting too, the
//! candidates bound at the first pass, the spend — is the caller's
//! (`fauna_client_folders::FoldersAuthor`'s launch pass), since only it holds
//! the device's custody. This walk never adopts a head outside the bound set
//! it is handed, so a nest that serves a head later cannot extend the
//! adoption ((11)(j), residual (3)).
//!
//! **Per bound head** (ruling (11)(d), term for term):
//!
//! - **Still undone?** The path's newest non-retention, non-refused row must
//!   still be the bound row, and still judge history. Anything else — this
//!   walk's own earlier record, another device's take-over, a newer edit — is
//!   skipped, never re-recorded.
//! - **Opened under its signer's roots only.** A head resting under a content
//!   key opens under the set's generations alone (no owner root is offered at
//!   all, current or retired), a plaintext head (a public set) needs no key;
//!   a head whose bytes or name open under none of its signer's roots is
//!   skipped and noted, never re-signed. A head resting under an owner root
//!   is **re-sealed** (`TakeOverWalk::reseal`): opened under its signer's
//!   own retired root and that root's predecessors alone, its bytes moved a
//!   window at a time under the set's current root, the new manifest recorded
//!   — the one shape whose bytes move, since a re-signed owner-root head would
//!   still open under the retired root only. A signer this device cannot
//!   place in its paired chain leaves the head owed.
//! - **As what.** Re-signed over the SAME manifest (no byte moves; the
//!   owner-root re-seal over its new one), as the
//!   current identity, under the live nonce, with a resolution stamp anchored
//!   at the head (every row of the path up to it was read; the bytes are its
//!   own), and **no `changes.supersede`**: the history row stays the version
//!   it is.
//! - **Deletes.** A history delete is re-recorded only over an older row
//!   signed outside the owner's chain (a member's row the cut resurfaced); on
//!   an owner-only set a history tombstone and an absent path read the same.

use std::sync::Arc;

use anyhow::Result;
use fauna_client_sync::SyncClient;
use fauna_client_sync::row_judge::{ProjectedRow, ReaderSeat};
use fauna_core::crypto::BackupKey;
use fauna_core::data::ContentHash;
use fauna_core::file_download::{BlobFetcher, FileDownloadKeys, PredecessorSealKey, RecordSigner};
use fauna_core::folder_keys::FolderKeyResolver;
use fauna_core::nest_reseal::{TakeOverCandidate, TakeOverPass};
use fauna_protocol::sync::SyncChange;
use fauna_protocol::sync_row_verify::RowVerdict;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::blob_uploader::{MediaBlobUploader, UploaderSink};

/// One seat's take-over walk: the change log and the signed record through
/// `sync`, the verified-row judge through `nest`, the set's custody through
/// `resolver`, the bytes through `fetcher`, and — for a head under an owner
/// root, the one shape whose bytes move — the re-sealed copy through
/// `uploader`.
pub struct TakeOverWalk<R: RpcRequester> {
    nest: R,
    /// Signs every record it writes as the current identity, under the live
    /// nonce `resolver` answers.
    sync: SyncClient<R>,
    seat: ReaderSeat,
    resolver: Arc<dyn FolderKeyResolver>,
    /// The recording device (the one the app's Media gestures record under).
    device_id: String,
    fetcher: Arc<dyn BlobFetcher>,
    uploader: Arc<dyn MediaBlobUploader>,
    /// The current owner root — a re-seal's target on an owner-only set, and
    /// never offered to open a predecessor-signed head (ruling (8)(c),
    /// [`RecordSigner::Predecessor`]).
    owner: BackupKey,
    /// The retired owner roots, each paired with its identity, nearest hop
    /// first — what a predecessor-signed head under an owner root opens under
    /// ([`Self::with_predecessor_chain`]). Empty: no such head opens, and each
    /// stays owed.
    predecessor_keys: Vec<PredecessorSealKey>,
}

/// One path's state as the walk judges it: its newest non-retention row that
/// is a record, that row's verdict, and whether the newest admitted row below
/// it was signed outside the owner's chain.
struct PathHead {
    row: SyncChange,
    verdict: RowVerdict,
    over_member: bool,
}

impl<R> TakeOverWalk<R>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass + core::fmt::Display,
{
    /// `keypair` is the seat's current identity: it signs the records and
    /// names the seat to the judge. `predecessors` are the account's attested
    /// predecessor ids (`AccountRegistry::attested_predecessor_actor_ids`) —
    /// what tells the judge a retired identity's row is the owner chain's.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        nest: R,
        keypair: &fauna_core::identity::ActorKeypair,
        resolver: Arc<dyn FolderKeyResolver>,
        device_id: String,
        fetcher: Arc<dyn BlobFetcher>,
        uploader: Arc<dyn MediaBlobUploader>,
        predecessors: Vec<[u8; 32]>,
    ) -> Self {
        let nonces = fauna_client_sync::SetNonceSource::Resolver(resolver.clone());
        let sync =
            SyncClient::new(nest.clone()).with_record_signing(fauna_client_sync::RecordSigning {
                signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
                    keypair,
                )),
                set_nonce: nonces.clone(),
            });
        Self {
            nest,
            sync,
            seat: ReaderSeat {
                own: Some(keypair.actor_id().0),
                nonces: Some(nonces),
                predecessors,
                ..Default::default()
            },
            resolver,
            device_id,
            fetcher,
            uploader,
            owner: BackupKey::derive(keypair.secret_bytes()),
            predecessor_keys: Vec::new(),
        }
    }

    /// The account's retired owner roots, each paired with the identity it
    /// belongs to, nearest hop first — the shape
    /// `AccountRegistry::predecessor_backup_keys_by_actor` returns. Only a
    /// paired root is ever offered to a head signed as that identity or a
    /// later one (ruling (8)(c)), and only to OPEN it: the re-seal commits to
    /// the current root.
    #[must_use]
    pub fn with_predecessor_chain(
        mut self,
        chain: Vec<(fauna_core::identity::ActorId, BackupKey)>,
    ) -> Self {
        self.predecessor_keys = PredecessorSealKey::chain(chain);
        self
    }

    /// Every path whose newest record judges history, with that head's seq.
    pub async fn history_heads(&self, folder: &str) -> Result<Vec<TakeOverCandidate>> {
        let mut out: Vec<TakeOverCandidate> = self
            .judged_heads(folder)
            .await?
            .into_iter()
            .filter(|(_, head)| self.is_candidate(head))
            .map(|(hash, head)| (hash, head.row.seq))
            .collect();
        out.sort();
        Ok(out)
    }

    /// Re-sign the `bound` heads still undone (module doc).
    pub async fn take_over(
        &self,
        folder: &str,
        bound: &[TakeOverCandidate],
    ) -> Result<TakeOverPass> {
        let mut pass = TakeOverPass::default();
        if bound.is_empty() {
            return Ok(pass);
        }
        let folder_r = fauna_core::log_redact::log_folder_name(folder);
        // The set's custody with NO owner root: a content-keyed head opens
        // under the set's generations alone, and nothing this walk carries
        // may reach an owner root (ruling (8)(c) — the signer is a retired
        // identity; the owner-root re-seal takes its own custody below).
        let (keys, foreign) =
            fauna_core::label_custody::LabelCustody::new(Some(self.resolver.clone()), None)
                .keys_for(folder)
                .await;
        if foreign.is_some() {
            // Not a set this device owns on its nest: nothing is its to adopt.
            pass.skipped = bound.len();
            return Ok(pass);
        }
        // The same custody WITH the owner family — the current root as the
        // re-seal's target and the retired ones as open candidates — for the
        // owner-root re-seal alone ([`Self::reseal`]), which opens every head
        // under its signer's roots only (`RecordSigner::Predecessor`).
        let (owner_keys, _) = fauna_core::label_custody::LabelCustody::new(
            Some(self.resolver.clone()),
            Some(self.owner.clone()),
        )
        .with_predecessor_keys(self.predecessor_keys.clone())
        .keys_for(folder)
        .await;
        let heads = self.judged_heads(folder).await?;
        for (hash, seq) in bound {
            let Some(head) = heads.get(hash) else {
                pass.skipped += 1;
                continue;
            };
            if head.row.seq != *seq || !self.is_candidate(head) {
                // Taken already — by this walk's earlier pass, another
                // device, or a newer edit — or no longer history.
                pass.skipped += 1;
                continue;
            }
            match self.carry(folder, &keys, &owner_keys, head).await {
                Carried::Rerecorded => pass.rerecorded += 1,
                Carried::Skipped => pass.skipped += 1,
                Carried::Owed => pass.owed += 1,
            }
        }
        tracing::info!(
            folder = %folder_r,
            bound = bound.len(),
            rerecorded = pass.rerecorded,
            skipped = pass.skipped,
            owed = pass.owed,
            "succession take-over (projection walk)"
        );
        Ok(pass)
    }

    /// A head this walk may take over: history, and — for a delete — one over
    /// a member's row the cut resurfaced.
    fn is_candidate(&self, head: &PathHead) -> bool {
        if !matches!(head.verdict, RowVerdict::History { .. }) {
            return false;
        }
        !is_delete(&head.row) || head.over_member
    }

    fn is_own_account(&self, signed_as: &[u8; 32]) -> bool {
        self.seat.own.as_ref() == Some(signed_as) || self.seat.predecessors.contains(signed_as)
    }

    /// The set's whole change log (every page), each path's rows judged in one
    /// batch and folded to the path's newest record.
    async fn judged_heads(
        &self,
        folder: &str,
    ) -> Result<std::collections::HashMap<String, PathHead>> {
        let mut rows: Vec<SyncChange> = Vec::new();
        let mut certs = Vec::new();
        let mut since = 0i64;
        loop {
            let page = self
                .sync
                .changes_list(Some(folder.to_string()), None, since, None)
                .await
                .map_err(|e| anyhow::anyhow!("take-over walk: fauna.sync.changes.list: {e}"))?;
            let Some(top) = page.changes.iter().map(|c| c.seq).max() else {
                break;
            };
            certs.extend(page.signer_certs);
            rows.extend(page.changes.into_iter().filter(|c| c.seq > since));
            if top <= since {
                break;
            }
            since = top;
        }
        rows.retain(|r| r.is_retention != Some(true));
        if rows.is_empty() {
            return Ok(Default::default());
        }
        let verdicts = {
            let projected: Vec<ProjectedRow<'_>> = rows
                .iter()
                .map(|row| ProjectedRow {
                    folder,
                    folder_hash: None,
                    folder_id: None,
                    row: Some(row.clone()),
                })
                .collect();
            self.seat
                .judge(&self.nest)
                .judge(&projected, &certs)
                .await
                .map_err(|e| anyhow::anyhow!("take-over walk: judging the set's rows: {e}"))?
        };
        let mut by_path: std::collections::HashMap<String, Vec<(SyncChange, RowVerdict)>> =
            std::collections::HashMap::new();
        for (row, verdict) in rows.into_iter().zip(verdicts) {
            by_path
                .entry(row.path_hash.to_ascii_lowercase())
                .or_default()
                .push((row, verdict));
        }
        let mut heads = std::collections::HashMap::new();
        for (hash, mut rows) in by_path {
            rows.sort_by_key(|(r, _)| std::cmp::Reverse(r.seq));
            let mut iter = rows.into_iter();
            // The newest record: a refused row is no record, and the walk
            // passes it as the cursor does.
            let Some((row, verdict)) = iter
                .by_ref()
                .find(|(_, v)| !matches!(v, RowVerdict::Refused(_)))
            else {
                continue;
            };
            let mut over_member = false;
            for (_, below) in iter {
                match below {
                    RowVerdict::Verified { signed_as, .. } => {
                        over_member = !self.is_own_account(&signed_as);
                        break;
                    }
                    RowVerdict::Exempt => break,
                    _ => continue,
                }
            }
            heads.insert(
                hash,
                PathHead {
                    row,
                    verdict,
                    over_member,
                },
            );
        }
        Ok(heads)
    }

    /// Open one head under its signer's roots and re-sign it — or, for a head
    /// under an owner root, hand it to [`Self::reseal`].
    async fn carry(
        &self,
        folder: &str,
        keys: &FileDownloadKeys,
        owner_keys: &FileDownloadKeys,
        head: &PathHead,
    ) -> Carried {
        let row = &head.row;
        let RowVerdict::History { signed_as, .. } = head.verdict else {
            return Carried::Skipped;
        };
        let keys = FileDownloadKeys {
            record_signer: RecordSigner::Predecessor(fauna_core::identity::ActorId(signed_as)),
            ..keys.clone()
        };
        let delete = is_delete(row);
        let stamped = row.content_key_version.is_some();
        // The name, under the same bound as the bytes: a stamped label opens
        // under the set's generations; a plaintext one is trusted only when
        // it hashes to the row's own signed `path_hash`. An unstamped SEALED
        // label rests under an owner root — the re-seal's.
        let plaintext = row.path.clone().filter(|p| !p.is_empty()).filter(|p| {
            hex::encode(fauna_core::sync::path_hash(p)).eq_ignore_ascii_case(&row.path_hash)
        });
        let path = match (&row.path_sealed, stamped) {
            (Some(sealed), true) => match fauna_core::label_custody::open_change_path(
                |generation| keys.label_open_roots(generation),
                Some(&sealed[..]),
                &row.path_hash,
            ) {
                fauna_core::label_custody::ChangePathOpen::Opened(p) => p,
                other => {
                    tracing::warn!(
                        seq = row.seq,
                        verdict = ?other,
                        "succession take-over: a history head whose name opens under none of \
                         its signer's roots; not carried"
                    );
                    return Carried::Skipped;
                }
            },
            (Some(_), false) if plaintext.is_none() => {
                return self.reseal(folder, owner_keys, head, signed_as, None).await;
            }
            _ => match plaintext {
                Some(p) => p,
                None => {
                    tracing::warn!(
                        seq = row.seq,
                        "succession take-over: a history head with no name this walk can trust; \
                         not carried"
                    );
                    return Carried::Skipped;
                }
            },
        };
        let path_r = fauna_core::log_redact::log_path(&path);

        let manifest_hex = if delete {
            None
        } else {
            let Some(manifest) = row
                .manifest_hash
                .as_deref()
                .and_then(|h| fauna_core::hex32::decode(h).ok())
                .map(ContentHash::from_digest_raw)
            else {
                tracing::warn!(
                    seq = row.seq,
                    "succession take-over: a head names no manifest"
                );
                return Carried::Skipped;
            };
            let stamp = row
                .content_key_version
                .map(fauna_core::blob_seal::SealStamp::Version);
            match fauna_core::file_download::fetch_manifest(
                self.fetcher.as_ref(),
                &keys,
                &manifest,
                stamp,
            )
            .await
            {
                // An unstamped head's bytes must rest in plaintext: sealed
                // ones are under an owner root, the re-seal's.
                Ok(m) if !stamped && m.is_sealed() => {
                    return self
                        .reseal(folder, owner_keys, head, signed_as, Some(path))
                        .await;
                }
                Ok(_) => {}
                // Unstamped and not opened with no owner root offered: an
                // owner-root seal — the re-seal opens it under its signer's
                // roots, or classifies why it cannot.
                Err(_) if !stamped => {
                    return self
                        .reseal(folder, owner_keys, head, signed_as, Some(path))
                        .await;
                }
                Err(e) => {
                    let bound = fauna_core::apply_failure::permanent_reason(&e)
                        == Some(
                            fauna_core::apply_failure::PermanentApplyFailure::SIGNER_BOUND.reason,
                        );
                    tracing::warn!(
                        path = %path_r,
                        error = %format!("{e:#}"),
                        "succession take-over: a head whose manifest does not open under its \
                         signer's roots; not carried"
                    );
                    // Refused for its bound: final (noted); anything else
                    // is retried.
                    return if !bound {
                        Carried::Owed
                    } else {
                        Carried::Skipped
                    };
                }
            }
            Some(hex::encode(manifest.digest()))
        };

        let change_type = if delete {
            "delete"
        } else {
            row.change_type.as_str()
        };
        let recorded = self
            .sync
            .changes_record(
                folder,
                self.device_id.clone(),
                path.clone(),
                manifest_hex,
                if delete { 0 } else { row.size_bytes },
                change_type,
                row.content_key_version,
                row.thumbnail_hash.clone(),
                // The label the head carries, already opened under its
                // generation above (a plaintext head carries none).
                if stamped {
                    row.path_sealed.as_ref().map(|b| b.to_vec())
                } else {
                    None
                },
                // A resolution stamp anchored at the head: no novel content,
                // and every row of the path up to it was read.
                Some(row.seq),
                Some(true),
                None,
                None,
            )
            .await;
        landed(recorded.map(|reply| reply.seq), &path_r, row.seq)
    }

    /// **The owner-root re-seal** (ruling (11)(l)): a bound head resting under
    /// an owner root is opened under the roots its signer is allowed alone
    /// (`RecordSigner::Predecessor` — the signer's own retired root and its
    /// predecessors', never the current one), its bytes re-sealed a window at
    /// a time under the set's CURRENT root through the shared
    /// [`fauna_core::nest_reseal::reseal_nest_copy_windowed`] (the whole
    /// source verified first, the manifest posted only once the running hash
    /// equals the head's address), and the new manifest recorded as the
    /// current identity with a resolution stamp at the head and no supersede.
    /// The current root is the one a fresh write of the set seals under: the
    /// owner root on an owner-only set, the current generation on a
    /// content-keyed one (its pre-bind content, part (D)). Resumable: the seal
    /// is deterministic, so a pass cut short after the byte move re-runs to
    /// the same manifest, the chunks it stored deduped. `path` is the name
    /// already opened by the caller, `None` for a sealed label still to open.
    async fn reseal(
        &self,
        folder: &str,
        owner_keys: &FileDownloadKeys,
        head: &PathHead,
        signed_as: [u8; 32],
        path: Option<String>,
    ) -> Carried {
        let row = &head.row;
        let signer = fauna_core::identity::ActorId(signed_as);
        if !self
            .predecessor_keys
            .iter()
            .any(|k| k.actor_id == Some(signer))
        {
            // This device cannot place the signer in its paired chain (its
            // retired seed has not reached this device's registry yet): no
            // root may be offered, and nothing is final — owed.
            tracing::info!(
                seq = row.seq,
                "succession take-over: a head under an owner root whose signer's root this \
                 device does not hold; owed"
            );
            return Carried::Owed;
        }
        let content_keyed = owner_keys.is_content_keyed();
        let keys = FileDownloadKeys {
            record_signer: RecordSigner::Predecessor(signer),
            // Part (D)'s unstamped open on a content-keyed set: the judge
            // verified the row as the owner chain's under the set's nonce.
            owner_signed_record: content_keyed,
            ..owner_keys.clone()
        };
        let path = match path {
            Some(path) => path,
            None => match fauna_core::label_custody::open_change_path(
                |generation| keys.label_open_roots(generation),
                row.path_sealed.as_ref().map(|b| &b[..]),
                &row.path_hash,
            ) {
                fauna_core::label_custody::ChangePathOpen::Opened(p) => p,
                other => {
                    tracing::warn!(
                        seq = row.seq,
                        verdict = ?other,
                        "succession take-over: a head under an owner root whose name opens \
                         under none of its signer's roots; not carried"
                    );
                    return Carried::Skipped;
                }
            },
        };
        let path_r = fauna_core::log_redact::log_path(&path);
        let seal_root = if content_keyed {
            let Some(content_keys) = owner_keys.content_keys.as_ref() else {
                // Content-keyed but this seat's custody does not hold the
                // keys yet: never a seal under an owner root (fail closed).
                return Carried::Owed;
            };
            (
                *content_keys.current_key(),
                Some(fauna_core::blob_seal::SealStamp::Version(
                    content_keys.current_version(),
                )),
            )
        } else {
            (self.owner.convergent_chunk_root(), None)
        };
        let generation = match seal_root.1 {
            Some(fauna_core::blob_seal::SealStamp::Version(v)) => Some(v),
            _ => None,
        };

        let (manifest_hex, size, thumbnail_hash) = if is_delete(row) {
            (None, 0, None)
        } else {
            let Some(manifest) = row
                .manifest_hash
                .as_deref()
                .and_then(|h| fauna_core::hex32::decode(h).ok())
                .map(ContentHash::from_digest_raw)
            else {
                tracing::warn!(
                    seq = row.seq,
                    "succession take-over: a head names no manifest"
                );
                return Carried::Skipped;
            };
            let sink = UploaderSink(self.uploader.as_ref());
            let resealed = match fauna_core::nest_reseal::reseal_nest_copy_windowed(
                self.fetcher.as_ref(),
                &keys,
                manifest,
                None,
                &path,
                Some(seal_root),
                &sink,
            )
            .await
            {
                Ok(resealed) => resealed,
                Err(e) => {
                    // A permanent refusal — the bytes open under none of the
                    // signer's roots, or do not address the head — is final
                    // and noted; a fetch, a store or a key still to arrive is
                    // retried next pass.
                    let final_ = fauna_core::apply_failure::permanent_reason(&e).is_some()
                        && !fauna_core::apply_failure::is_key_pending(&e);
                    tracing::warn!(
                        path = %path_r,
                        error = %format!("{e:#}"),
                        final_,
                        "succession take-over: a head under an owner root was not re-sealed"
                    );
                    return if final_ {
                        Carried::Skipped
                    } else {
                        Carried::Owed
                    };
                }
            };
            // The thumbnail moves with its file, under the same bound and
            // root; one that does not re-seal is dropped (the download
            // backfill re-renders it), never a reason to hold the file.
            let thumbnail_hash = match row
                .thumbnail_hash
                .as_deref()
                .and_then(|h| fauna_core::hex32::decode(h).ok())
            {
                Some(thumb) => fauna_core::nest_reseal::reseal_nest_copy_windowed(
                    self.fetcher.as_ref(),
                    &keys,
                    ContentHash::from_digest_raw(thumb),
                    None,
                    &path,
                    Some(seal_root),
                    &sink,
                )
                .await
                .inspect_err(|e| {
                    tracing::warn!(
                        path = %path_r,
                        error = %format!("{e:#}"),
                        "succession take-over: the head's thumbnail did not re-seal; recorded \
                         without one"
                    );
                })
                .ok()
                .map(|t| hex::encode(t.manifest_hash.digest())),
                None => None,
            };
            (
                Some(hex::encode(resealed.manifest_hash.digest())),
                resealed.manifest.total_size as i64,
                thumbnail_hash,
            )
        };

        // The name seals under the same root the chunks did — the current one
        // (`label_seal_root` reads the current root alone).
        let path_sealed = fauna_core::label_custody::seal_path_from_keys(owner_keys, &path).ok();
        let recorded = self
            .sync
            .changes_record(
                folder,
                self.device_id.clone(),
                path.clone(),
                manifest_hex.clone(),
                size,
                if manifest_hex.is_none() {
                    "delete"
                } else {
                    "modify"
                },
                generation,
                thumbnail_hash,
                path_sealed,
                // A resolution stamp anchored at the head: no novel content,
                // and every row of the path up to it was read.
                Some(row.seq),
                Some(true),
                None,
                None,
            )
            .await;
        landed(recorded.map(|reply| reply.seq), &path_r, row.seq)
    }
}

/// What a take-over record's answer means for the pass: a new row above the
/// head is carried; anything else is retried next pass.
fn landed<E: core::fmt::Display>(
    recorded: core::result::Result<i64, E>,
    path_r: &dyn core::fmt::Display,
    head_seq: i64,
) -> Carried {
    match recorded {
        Ok(seq) if seq > head_seq => Carried::Rerecorded,
        Ok(seq) => {
            tracing::warn!(
                path = %path_r,
                seq,
                head_seq,
                "succession take-over was answered with no new row; retried next pass"
            );
            Carried::Owed
        }
        Err(e) => {
            tracing::warn!(
                path = %path_r,
                error = %e,
                "succession take-over record did not land; retried next pass"
            );
            Carried::Owed
        }
    }
}

enum Carried {
    Rerecorded,
    Skipped,
    Owed,
}

fn is_delete(row: &SyncChange) -> bool {
    row.manifest_hash.is_none() || row.change_type.eq_ignore_ascii_case("delete")
}

// ── Native (`Arc<NestClient>`) ──────────────────────────────────────────────
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use fauna_client::{NestClient, NestPublicChunkFetcher};

    #[async_trait::async_trait]
    impl fauna_core::nest_reseal::SuccessionTakeOver for TakeOverWalk<Arc<NestClient>> {
        async fn history_heads(&self, folder: &str) -> Result<Vec<TakeOverCandidate>> {
            TakeOverWalk::history_heads(self, folder).await
        }

        async fn take_over(
            &self,
            folder: &str,
            bound: &[TakeOverCandidate],
        ) -> Result<TakeOverPass> {
            TakeOverWalk::take_over(self, folder, bound).await
        }
    }

    /// The walk over `nest`'s authenticated session, with the same byte
    /// seams the Media machine injects. `predecessor_keys` as
    /// [`TakeOverWalk::with_predecessor_chain`].
    pub fn take_over_walk(
        nest: Arc<NestClient>,
        keypair: &fauna_core::identity::ActorKeypair,
        resolver: Arc<dyn FolderKeyResolver>,
        device_id: String,
        predecessors: Vec<[u8; 32]>,
        predecessor_keys: Vec<(fauna_core::identity::ActorId, BackupKey)>,
    ) -> TakeOverWalk<Arc<NestClient>> {
        let fetcher: Arc<dyn BlobFetcher> = Arc::new(NestPublicChunkFetcher::new(&nest));
        let uploader: Arc<dyn MediaBlobUploader> =
            Arc::new(crate::blob_uploader::NativeBlobUploader::new(&nest));
        TakeOverWalk::new(
            nest,
            keypair,
            resolver,
            device_id,
            fetcher,
            uploader,
            predecessors,
        )
        .with_predecessor_chain(predecessor_keys)
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::take_over_walk;

// ── Wasm (`WsRpcClient`) ────────────────────────────────────────────────────
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use fauna_rpc_wasm::WsRpcClient;

    #[async_trait::async_trait(?Send)]
    impl fauna_core::nest_reseal::SuccessionTakeOver for TakeOverWalk<WsRpcClient> {
        async fn history_heads(&self, folder: &str) -> Result<Vec<TakeOverCandidate>> {
            TakeOverWalk::history_heads(self, folder).await
        }

        async fn take_over(
            &self,
            folder: &str,
            bound: &[TakeOverCandidate],
        ) -> Result<TakeOverPass> {
            TakeOverWalk::take_over(self, folder, bound).await
        }
    }

    /// The walk over the SPA's browser session — the wasm twin of the native
    /// builder.
    pub fn take_over_walk(
        nest: WsRpcClient,
        keypair: &fauna_core::identity::ActorKeypair,
        resolver: Arc<dyn FolderKeyResolver>,
        device_id: String,
        predecessors: Vec<[u8; 32]>,
        predecessor_keys: Vec<(fauna_core::identity::ActorId, BackupKey)>,
    ) -> TakeOverWalk<WsRpcClient> {
        let fetcher: Arc<dyn BlobFetcher> = Arc::new(
            fauna_core::file_download::WasmPublicChunkFetcher::new(nest.nest_url()),
        );
        let uploader: Arc<dyn MediaBlobUploader> =
            Arc::new(crate::blob_uploader::WasmBlobUploader::new(nest.clone()));
        TakeOverWalk::new(
            nest,
            keypair,
            resolver,
            device_id,
            fetcher,
            uploader,
            predecessors,
        )
        .with_predecessor_chain(predecessor_keys)
    }
}
#[cfg(target_arch = "wasm32")]
pub use wasm::take_over_walk;

#[cfg(test)]
#[path = "succession_take_over_tests.rs"]
mod tests;
