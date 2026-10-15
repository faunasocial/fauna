//! **Third-party deposit adoption** — the seat's half of the deposit ingress
//! (`docs/goal/behavior/folder-deposit.md`).
//!
//! The nest parks a third party's deposit sealed to the folder owner's
//! recipient key, in the folder's inbox segment. Adoption is the owner's next
//! catch-up: list the parked items (`fauna.folders.deposits.list`), open each
//! with the recipient secret derived from the mail custody's MSEK — the X-Wing
//! opener, which also opens a classical seal — land it in the sync root through
//! the same write door every remotely-authored change takes
//! ([`fauna_core::path_guard::contained_apply_target`], then
//! [`crate::atomic_write::atomic_write_file`]'s rename: guard, then rename,
//! never a direct write), record it as an ordinary own change
//! ([`SyncEngine::upload_file`] — the upload seals it under the folder's own
//! scheme, `BackupKey`-rooted or the M2 content key), and retire the item
//! (`fauna.folders.deposits.retire`) once that change row is durable.
//!
//! An item larger than one message carries no body in its envelope, only the
//! count of its parts: adoption fetches each (`fauna.folders.deposits.part`),
//! opens it with the same key, checks the index sealed inside it, and streams
//! it in order into a stage — a dotfile temp beside the target, hashed as it
//! is written — which lands by the guarded rename, so no larger body is ever
//! held in memory whole.
//!
//! **Idempotent on the deposit id, across seats and across crashes.** The
//! item lands under its own name, or — when another file already holds that
//! name — under a name that carries the deposit id, so every seat computes the
//! same target for the same item. A seat that finds the target already
//! holding exactly the item's bytes (another seat adopted it, or this one
//! crashed before the retire) adopts nothing new: it records if the row is
//! not yet durable, then retires. A retire answering "already gone" is
//! success.
//!
//! **Only the owner's seats adopt.** The item is sealed to the owner's
//! recipient key, which a member never holds; an engine is armed with an
//! inbox ([`SyncEngine::set_deposit_inbox`]) only for a set the account owns
//! on its own nest. A member sees the adopted file as it sees any other.

use std::sync::Arc;

use anyhow::{Context, Result};
use fauna_core::data::ContentHash;
use fauna_protocol::folders::{
    DepositEnvelope, DepositPart, FolderDepositsListRequest, FolderDepositsPartRequest,
    FolderDepositsRetireRequest, MAX_DEPOSIT_PARTS, ParkedDeposit, is_deposit_name,
};

use super::SyncEngine;

/// What arms an engine to adopt: the set's row id on its own nest (the inbox
/// kinds name it) and the custody source the recipient key derives from.
pub(crate) struct DepositInbox {
    pub(crate) folder_id: i64,
    pub(crate) keys: Arc<dyn fauna_client_folders::FolderKeyReader>,
}

/// What one adoption pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DepositAdoption {
    /// Items landed and recorded by this pass and retired.
    pub adopted: usize,
    /// Items found already landed (another seat, or an earlier pass) and
    /// retired.
    pub already_landed: usize,
    /// Items left parked for a later pass (unopenable, no free name, the
    /// record not yet durable).
    pub deferred: usize,
    /// The path of every item this pass retired (adopted or found landed), in
    /// order — what an on-demand root flips to the platform's in-sync mark,
    /// as it flips every other recorded upload.
    pub landed: Vec<String>,
}

/// Where one item lands, and whether its bytes are already there.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// Nothing holds this name: write the item here.
    Fresh(String),
    /// This name already holds exactly the item's bytes.
    Landed(String),
}

/// The name an item lands under when another file already holds its own:
/// the deposit id before the extension — `report (deposit 17).pdf` — so every
/// seat derives the same one.
fn deposit_variant(name: &str, id: i64) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} (deposit {id}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} (deposit {id})"),
    }
}

/// The recipient secret every MSEK generation derives, concatenated — the
/// `k × (x25519 ∥ ml-kem dk)` key [`open_deposit`] trial-opens with.
fn recipient_key(mseks: &[fauna_core::secret::SecretArray32]) -> zeroize::Zeroizing<Vec<u8>> {
    let mut key = zeroize::Zeroizing::new(Vec::new());
    for msek in mseks {
        key.extend_from_slice(
            &fauna_mls::wrapped_blob::derive_recipient_mail_capability_secret(msek),
        );
    }
    key
}

/// Open one recipient-sealed record with the owner's recipient key — the
/// hybrid (X-Wing) opener, which opens a classical seal too.
fn open_sealed<T: serde::de::DeserializeOwned>(sealed: &[u8], key: &[u8]) -> Result<T> {
    let envelope = fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(sealed)
        .map_err(|e| anyhow::anyhow!("decode the sealed deposit: {e}"))?;
    let plaintext = fauna_mls::wrapped_blob::unseal_mail_record_with_derived_key(&envelope, key)
        .map_err(|e| anyhow::anyhow!("open the sealed deposit: {e}"))?;
    fauna_protocol::decode_strict(&plaintext).context("decode the deposit envelope")
}

/// Open one parked item's envelope with the owner's recipient key.
pub(crate) fn open_deposit(sealed: &[u8], key: &[u8]) -> Result<DepositEnvelope> {
    open_sealed(sealed, key)
}

/// The prefix of an item's staged body: a dotfile, which the scanner never
/// uploads (`watcher::scan_recursive_filtered`), beside the target so the
/// landing rename stays on one filesystem.
const STAGE_PREFIX: &str = ".fauna-deposit-";

/// An item's body as the pass holds it: a single-message item's bytes from
/// its envelope (at most one message), or a larger item's parts streamed to a
/// temp file beside the target ([`PartWriter`]) — never the whole of a larger
/// body in memory.
enum Body<'a> {
    Inline(&'a [u8]),
    Staged(Staged),
}

impl Body<'_> {
    fn len(&self) -> u64 {
        match self {
            Self::Inline(bytes) => bytes.len() as u64,
            Self::Staged(staged) => staged.len,
        }
    }
}

/// A larger item's body, staged on disk: dropped unlanded, the file goes too.
struct Staged {
    temp: tempfile::NamedTempFile,
    hash: ContentHash,
    len: u64,
}

/// Streams an item's opened parts, in index order, into its stage — hashing
/// as it writes, so the landed check needs no second read of the body.
struct PartWriter {
    temp: tempfile::NamedTempFile,
    file: tokio::fs::File,
    hasher: blake3::Hasher,
    len: u64,
}

impl PartWriter {
    fn new_in(dir: &std::path::Path) -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix(STAGE_PREFIX)
            .tempfile_in(dir)
            .context("creating the deposit's stage")?;
        let file = tokio::fs::File::from_std(temp.reopen().context("opening the deposit's stage")?);
        Ok(Self {
            temp,
            file,
            hasher: blake3::Hasher::new(),
            len: 0,
        })
    }

    /// Append part `index` — refused when the index sealed inside it is not
    /// its place.
    async fn push(&mut self, index: u32, part: DepositPart) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        if part.index != index {
            anyhow::bail!("deposit part {index} opened as part {}", part.index);
        }
        self.file
            .write_all(&part.body)
            .await
            .context("writing the deposit's stage")?;
        self.hasher.update(&part.body);
        self.len += part.body.len() as u64;
        Ok(())
    }

    /// Flush and fsync the stage — durable before any rename lands it.
    async fn finish(mut self) -> Result<Staged> {
        use tokio::io::AsyncWriteExt;
        self.file
            .flush()
            .await
            .context("flushing the deposit's stage")?;
        self.file
            .sync_all()
            .await
            .context("fsync the deposit's stage")?;
        drop(self.file);
        Ok(Staged {
            temp: self.temp,
            hash: ContentHash::from_digest_raw(*self.hasher.finalize().as_bytes()),
            len: self.len,
        })
    }
}

/// The body of an item larger than one message: each of its `parts` fetched
/// and opened in index order — the index inside each seal must match its
/// place — and streamed into a stage in `dir`, at most one part held at a
/// time. [`MAX_DEPOSIT_PARTS`] bounds the count.
async fn reassemble(
    engine: &SyncEngine,
    folder_id: i64,
    item: &ParkedDeposit,
    parts: u32,
    key: &[u8],
    dir: &std::path::Path,
) -> Result<Staged> {
    if parts > MAX_DEPOSIT_PARTS {
        anyhow::bail!("the deposit claims {parts} parts, more than any deposit may have");
    }
    let control = Arc::clone(&*engine.control.read().unwrap());
    let mut writer = PartWriter::new_in(dir)?;
    for index in 0..parts {
        let sealed = control
            .fetch_deposit_part(FolderDepositsPartRequest {
                folder_id,
                deposit_id: item.id,
                index,
                extra: Default::default(),
            })
            .await?;
        writer.push(index, open_sealed(&sealed, key)?).await?;
    }
    writer.finish().await
}

/// Does `path` hold exactly the `len` bytes hashing to `hash`? Length first,
/// then a streamed hash — never the file read whole. `Err` carries the open's
/// error kind (`NotFound` is a free name to the caller).
fn holds_exactly(path: &std::path::Path, len: u64, hash: &ContentHash) -> std::io::Result<bool> {
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() != len {
        return Ok(false);
    }
    let digest = fauna_core::chunker_stream::blake3_of_reader(file)?;
    Ok(ContentHash::from_digest_raw(digest) == *hash)
}

impl SyncEngine {
    /// Arm this engine to adopt the set's parked third-party deposits — the
    /// build does so only for a set the account owns on its own nest.
    pub fn set_deposit_inbox(
        &self,
        folder_id: i64,
        keys: Arc<dyn fauna_client_folders::FolderKeyReader>,
    ) {
        *self.deposit_inbox.write().unwrap() = Some(Arc::new(DepositInbox { folder_id, keys }));
    }

    /// One adoption pass over the set's inbox (module doc). A no-op for an
    /// engine no inbox armed, a read-only or metadata-only one, or an account
    /// whose mail custody holds no MSEK (nothing could have been sealed to a
    /// recipient key it never published). An `Err` is a pass to retry: nothing
    /// is retired that is not durable.
    pub async fn adopt_deposits(&self) -> Result<DepositAdoption> {
        let mut done = DepositAdoption::default();
        let Some(inbox) = self.deposit_inbox.read().unwrap().clone() else {
            return Ok(done);
        };
        if self.is_read_only() || self.is_metadata_only_residency() {
            return Ok(done);
        }
        let control = Arc::clone(&*self.control.read().unwrap());
        let mut after = 0;
        let mut key = None;
        loop {
            let page = control
                .list_deposits(FolderDepositsListRequest {
                    folder_id: inbox.folder_id,
                    after,
                    extra: Default::default(),
                })
                .await?;
            if page.items.is_empty() {
                return Ok(done);
            }
            // The custody read waits until an item is actually parked.
            if key.is_none() {
                let mseks = inbox.keys.recipient_mseks().await?;
                if mseks.is_empty() {
                    tracing::debug!("deposits parked but no MSEK held here; another seat adopts");
                    return Ok(done);
                }
                key = Some(recipient_key(&mseks));
            }
            let key = key.as_deref().expect("set above");
            for item in &page.items {
                after = after.max(item.id);
                match self.adopt_one(inbox.folder_id, item, key).await {
                    Ok(Some((fresh, path))) => {
                        if fresh {
                            done.adopted += 1;
                        } else {
                            done.already_landed += 1;
                        }
                        done.landed.push(path);
                    }
                    Ok(None) => done.deferred += 1,
                    Err(e) => {
                        done.deferred += 1;
                        tracing::warn!(deposit = item.id, "deposit adoption deferred: {e:#}");
                    }
                }
            }
            if !page.more {
                return Ok(done);
            }
        }
    }

    /// Adopt one item: `Some((true, path))` landed at `path` and retired by
    /// this call, `Some((false, path))` found already landed there and
    /// retired, `None` left parked.
    async fn adopt_one(
        &self,
        folder_id: i64,
        item: &ParkedDeposit,
        key: &[u8],
    ) -> Result<Option<(bool, String)>> {
        let envelope = open_deposit(&item.sealed, key)?;
        // The nest's door checked the name; the seat checks again, as every
        // remote door re-guards what it materializes.
        if !is_deposit_name(&envelope.name) {
            anyhow::bail!("the deposit's name is not one plain file-name component");
        }
        let (body, hash) = if envelope.parts == 0 {
            let inline = envelope.body.as_ref();
            (Body::Inline(inline), ContentHash::of_raw(inline))
        } else {
            // A larger item streams into a stage beside its target: the
            // guarded own name's directory, which its deposit variant shares.
            let own =
                fauna_core::path_guard::contained_apply_target(&self.watch_dir, &envelope.name)?;
            let dir = own
                .parent()
                .context("the deposit's target has no directory")?;
            tokio::fs::create_dir_all(dir).await?;
            let staged = reassemble(self, folder_id, item, envelope.parts, key, dir).await?;
            let hash = staged.hash;
            (Body::Staged(staged), hash)
        };
        let Some(target) = self.deposit_target(&envelope.name, item.id, body.len(), &hash)? else {
            tracing::warn!(
                deposit = item.id,
                "no free name to adopt the deposit under; left parked"
            );
            return Ok(None);
        };
        let (path, fresh) = match target {
            Target::Fresh(path) => {
                let full = fauna_core::path_guard::contained_apply_target(&self.watch_dir, &path)?;
                if let Some(parent) = full.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                match body {
                    Body::Inline(bytes) => {
                        crate::atomic_write::atomic_write_file(&full, bytes).await?;
                    }
                    // Guard, then rename: the stage is already durable.
                    Body::Staged(staged) => {
                        staged
                            .temp
                            .persist(&full)
                            .map_err(|e| anyhow::anyhow!("landing the deposit's stage: {e}"))?;
                    }
                }
                (path, true)
            }
            // Already there: an unlanded stage goes with `body`.
            Target::Landed(path) => (path, false),
        };
        if !self.recorded_as(&path, &hash) && !self.upload_file(&path).await?.recorded {
            // On disk, not yet recorded: the next pass (or the watcher's
            // upload) finds it landed and retires it once durable.
            return Ok(None);
        }
        control_retire(self, folder_id, item.id).await?;
        tracing::info!(
            deposit = item.id,
            path = %fauna_core::log_redact::log_path(&path),
            fresh,
            "third-party deposit adopted"
        );
        Ok(Some((fresh, path)))
    }

    /// Is `path`'s recorded head exactly `hash` — the item's change row
    /// durable on the nest?
    fn recorded_as(&self, path: &str, hash: &ContentHash) -> bool {
        self.db
            .get_entry(path)
            .ok()
            .flatten()
            .is_some_and(|e| e.recorded_content_hash.as_ref() == Some(hash))
    }

    /// Where item `id` lands: its own name, else its deposit-id variant —
    /// whichever first is free or already holds exactly these bytes.
    fn deposit_target(
        &self,
        name: &str,
        id: i64,
        len: u64,
        hash: &ContentHash,
    ) -> Result<Option<Target>> {
        for candidate in [name.to_string(), deposit_variant(name, id)] {
            let entry = self.db.get_entry(&candidate)?;
            // Read only — the write goes through the guarded door.
            let full = self.watch_dir.join(&candidate);
            let entry_holds = entry.as_ref().is_some_and(|e| {
                e.recorded_content_hash.as_ref() == Some(hash)
                    || e.local_hash.as_ref() == Some(hash)
            });
            // A cloud placeholder (an on-demand root's file whose bytes are not
            // on this disk) is never opened: the read would wait on a fetch only
            // the root's own driving loop answers — the loop running this pass.
            // Its row says what it holds.
            if std::fs::symlink_metadata(&full)
                .is_ok_and(|m| crate::placeholder::is_cloud_placeholder(&m))
            {
                if entry_holds {
                    return Ok(Some(Target::Landed(candidate)));
                }
                continue;
            }
            match holds_exactly(&full, len, hash) {
                Ok(true) => return Ok(Some(Target::Landed(candidate))),
                // Not on disk but its row holds these bytes: a download the
                // pull owes, not a free name.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && entry_holds => {
                    return Ok(Some(Target::Landed(candidate)));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && entry.is_none() => {
                    return Ok(Some(Target::Fresh(candidate)));
                }
                // Other bytes, another file's row, or unreadable (a directory,
                // a placeholder that will not open): occupied.
                _ => {}
            }
        }
        Ok(None)
    }
}

async fn control_retire(engine: &SyncEngine, folder_id: i64, deposit_id: i64) -> Result<()> {
    let control = Arc::clone(&*engine.control.read().unwrap());
    // `false` — another seat retired it first — is success here.
    control
        .retire_deposit(FolderDepositsRetireRequest {
            folder_id,
            deposit_id,
            extra: Default::default(),
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(index: u32, body: &[u8]) -> DepositPart {
        DepositPart {
            index,
            body: body.to_vec().into(),
            ..Default::default()
        }
    }

    /// The writer streams each part to a temp file beside the target and
    /// hashes as it goes: the staged body is the concatenation, on disk, and
    /// its hash and length are the whole body's — no `Vec` of it is ever held.
    #[tokio::test]
    async fn the_writer_stages_the_parts_in_a_temp_file_beside_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = PartWriter::new_in(dir.path()).unwrap();
        for (index, body) in [&b"first-"[..], b"second-", b"third"].iter().enumerate() {
            writer
                .push(
                    u32::try_from(index).unwrap(),
                    part(u32::try_from(index).unwrap(), body),
                )
                .await
                .unwrap();
        }
        let staged = writer.finish().await.unwrap();
        assert_eq!(staged.len, 18);
        assert_eq!(staged.hash, ContentHash::of_raw(b"first-second-third"));
        let temp = staged.temp.path().to_path_buf();
        assert_eq!(
            temp.parent(),
            Some(dir.path()),
            "beside the target, for the rename"
        );
        assert!(
            temp.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".fauna-deposit-"),
            "a dotfile, which the scanner never uploads"
        );
        assert_eq!(std::fs::read(&temp).unwrap(), b"first-second-third");
        drop(staged);
        assert!(!temp.exists(), "an unlanded stage is removed with it");
    }

    /// A part whose sealed index is not its place refuses the item, and the
    /// half-written stage goes with the writer.
    #[tokio::test]
    async fn the_writer_refuses_a_part_out_of_its_place_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = PartWriter::new_in(dir.path()).unwrap();
        writer.push(0, part(0, b"a")).await.unwrap();
        assert!(writer.push(1, part(0, b"b")).await.is_err());
        drop(writer);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// The landed check compares length, then a streamed hash — never the
    /// whole file read into memory beside the whole body.
    #[test]
    fn the_landed_check_compares_length_then_a_streamed_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.bin");
        std::fs::write(&file, b"exact bytes").unwrap();
        let hash = ContentHash::of_raw(b"exact bytes");
        assert!(holds_exactly(&file, 11, &hash).unwrap());
        assert!(!holds_exactly(&file, 12, &hash).unwrap(), "another length");
        assert!(
            !holds_exactly(&file, 11, &ContentHash::of_raw(b"other bytes")).unwrap(),
            "same length, other bytes"
        );
        assert_eq!(
            holds_exactly(&dir.path().join("absent"), 11, &hash)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(
            holds_exactly(dir.path(), 11, &hash).map_or(true, |held| !held),
            "a directory never holds the item"
        );
    }

    #[test]
    fn the_variant_carries_the_deposit_id_before_the_extension() {
        assert_eq!(deposit_variant("report.pdf", 17), "report (deposit 17).pdf");
        assert_eq!(deposit_variant("README", 3), "README (deposit 3)");
        assert_eq!(deposit_variant(".hidden", 3), ".hidden (deposit 3)");
        assert_eq!(
            deposit_variant("a.tar.gz", 9),
            "a.tar (deposit 9).gz",
            "the last extension, as a file manager's copy name keeps it"
        );
    }
}
