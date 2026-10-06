//! **Third-party deposit adoption** — the seat's half of the deposit ingress
//! (`docs/goal/behavior/file-sync.md` § Third-party deposit ingress).
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
    DepositEnvelope, FolderDepositsListRequest, FolderDepositsRetireRequest, ParkedDeposit,
    is_deposit_name,
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

/// Open one parked item with the owner's recipient key — the hybrid (X-Wing)
/// opener, which opens a classical seal too.
pub(crate) fn open_deposit(sealed: &[u8], key: &[u8]) -> Result<DepositEnvelope> {
    let envelope = fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(sealed)
        .map_err(|e| anyhow::anyhow!("decode the sealed deposit: {e}"))?;
    let plaintext = fauna_mls::wrapped_blob::unseal_mail_record_with_derived_key(&envelope, key)
        .map_err(|e| anyhow::anyhow!("open the sealed deposit: {e}"))?;
    fauna_protocol::decode_strict(&plaintext).context("decode the deposit envelope")
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
                    Ok(Some(true)) => done.adopted += 1,
                    Ok(Some(false)) => done.already_landed += 1,
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

    /// Adopt one item: `Some(true)` landed and retired by this call,
    /// `Some(false)` found already landed and retired, `None` left parked.
    async fn adopt_one(
        &self,
        folder_id: i64,
        item: &ParkedDeposit,
        key: &[u8],
    ) -> Result<Option<bool>> {
        let envelope = open_deposit(&item.sealed, key)?;
        // The nest's door checked the name; the seat checks again, as every
        // remote door re-guards what it materializes.
        if !is_deposit_name(&envelope.name) {
            anyhow::bail!("the deposit's name is not one plain file-name component");
        }
        let body = envelope.body.as_ref();
        let hash = ContentHash::of_raw(body);
        let Some(target) = self.deposit_target(&envelope.name, item.id, body, &hash)? else {
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
                crate::atomic_write::atomic_write_file(&full, body).await?;
                (path, true)
            }
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
        Ok(Some(fresh))
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
        body: &[u8],
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
            match std::fs::read(&full) {
                Ok(bytes) if bytes == body => return Ok(Some(Target::Landed(candidate))),
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
