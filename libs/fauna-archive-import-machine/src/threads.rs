//! The `Archive` rail's read side (`docs/goal/behavior/archive-import.md`
//! § What each category becomes, the message-threads row): every imported
//! archive's message threads, read from the folders' `model/threads.cbor` and
//! `model/messages.cbor`, for the conversations manager's read-only
//! projection. Threads are model-only (never authored, unselectable in the
//! Scope step), so every thread an archive folder holds is listed.
//!
//! The read is plain over [`ArchiveNest`] and returns crate-local records, so
//! it is tested over the in-memory fake and needs no conversations graph;
//! with the `conversations-sink` feature, [`ArchiveThreads`] is the session's
//! `ArchiveThreadSource` over it.

use std::collections::{BTreeMap, BTreeSet};

use fauna_archive::model::{ArchiveMessage, ArchiveThread};
use fauna_archive::{Category, Entity, ExternalId};

use crate::link::same_person;
use crate::nest::{ArchiveNest, ArchiveNestError};
use crate::state::model_path;

/// One message as the rail shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedMessage {
    pub external_id: String,
    pub sender_name: String,
    pub is_owner: bool,
    pub created_at_ms: i64,
    pub text: String,
}

/// One imported thread with its messages, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedThread {
    /// `<platform>:<id>` — the thread's external id, stable across re-imports.
    pub key: String,
    /// The platform token (`fauna_core::source`'s vocabulary).
    pub platform: String,
    pub title: Option<String>,
    /// Every participant but the owner, by display name.
    pub participants: Vec<String>,
    pub messages: Vec<ImportedMessage>,
}

fn key_of(id: &ExternalId) -> String {
    format!("{}:{}", id.platform.token(), id.id)
}

fn micros_to_ms(micros: u64) -> i64 {
    i64::try_from(micros / 1000).unwrap_or(i64::MAX)
}

/// Every thread across the account's archive folders, each external id once —
/// the newest folder's copy wins, so a re-import's second folder repeats
/// rather than adds. A folder of a platform this build cannot name, or whose
/// model files are missing or unreadable, contributes nothing.
pub async fn read_imported_threads(
    nest: &dyn ArchiveNest,
) -> Result<Vec<ImportedThread>, ArchiveNestError> {
    let mut folders = nest.list_archive_folders().await?;
    folders.retain(|f| f.marker.platform.is_known());
    folders.sort_by_key(|f| std::cmp::Reverse(f.marker.created_at));

    let mut threads: BTreeMap<String, (ArchiveThread, fauna_archive::ExternalActorRef)> =
        BTreeMap::new();
    let mut messages: BTreeMap<String, Vec<ArchiveMessage>> = BTreeMap::new();
    let mut seen_messages = BTreeSet::new();
    for folder in &folders {
        for entity in read_model(nest, &folder.folder, &Category::Threads).await? {
            if let Entity::Thread(thread) = entity {
                threads
                    .entry(key_of(&thread.external_id))
                    .or_insert_with(|| (thread, folder.marker.owner.clone()));
            }
        }
        for entity in read_model(nest, &folder.folder, &Category::Messages).await? {
            if let Entity::Message(message) = entity
                && seen_messages.insert(key_of(&message.external_id))
            {
                messages
                    .entry(key_of(&message.thread))
                    .or_default()
                    .push(message);
            }
        }
    }

    Ok(threads
        .into_iter()
        .map(|(key, (thread, owner))| {
            let mut own = messages.remove(&key).unwrap_or_default();
            own.sort_by_key(|m| m.created_at);
            ImportedThread {
                platform: thread.external_id.platform.token().to_string(),
                title: thread.title.clone(),
                participants: thread
                    .participants
                    .iter()
                    .filter(|p| !same_person(p, &owner))
                    .map(|p| p.display_name.clone())
                    .collect(),
                messages: own
                    .into_iter()
                    .map(|m| ImportedMessage {
                        external_id: key_of(&m.external_id),
                        is_owner: same_person(&m.sender, &owner),
                        sender_name: m.sender.display_name,
                        created_at_ms: micros_to_ms(m.created_at.0),
                        text: m.text.unwrap_or_default(),
                    })
                    .collect(),
                key,
            }
        })
        .collect())
}

/// One category's model file, or nothing when it is absent or undecodable
/// (the folder is the truth; an unreadable file hides its threads, never
/// fails the others).
pub(crate) async fn read_model(
    nest: &dyn ArchiveNest,
    folder: &str,
    category: &Category,
) -> Result<Vec<Entity>, ArchiveNestError> {
    let Some(bytes) = nest.read_file(folder, &model_path(category)).await? else {
        return Ok(Vec::new());
    };
    match fauna_cbor::decode_strict::<Vec<Entity>>(&bytes) {
        Ok(entities) => Ok(entities),
        Err(e) => {
            tracing::warn!(%folder, "unreadable {}: {e}", model_path(category));
            Ok(Vec::new())
        }
    }
}

/// The session's `ArchiveThreadSource` over the account's archive folders.
#[cfg(feature = "conversations-sink")]
pub struct ArchiveThreads(pub std::sync::Arc<dyn ArchiveNest>);

/// Register `archive`'s threads as `manager`'s `Archive` rail source — the one
/// call every face makes (tui and linux at login, the FFI conversations
/// factory, the web core), over whatever transport built `archive`, so no
/// face constructs [`ArchiveThreads`] itself. On the manager, which every face
/// holds (web drives it without a session). Reads nothing: the face re-reads
/// through `ConversationsManager::refresh_archive_threads`.
#[cfg(feature = "conversations-sink")]
pub fn register_archive_threads(
    manager: &fauna_conversations::ConversationsManager,
    archive: std::sync::Arc<dyn ArchiveNest>,
) {
    manager.set_archive_thread_source(std::sync::Arc::new(ArchiveThreads(archive)));
}

#[cfg(feature = "conversations-sink")]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl fauna_conversations::backend::ArchiveThreadSource for ArchiveThreads {
    async fn archive_threads(
        &self,
    ) -> Result<Vec<fauna_conversations::backend::ArchiveThreadRecord>, String> {
        use fauna_conversations::backend::{ArchiveMessageRecord, ArchiveThreadRecord};
        let threads = read_imported_threads(self.0.as_ref())
            .await
            .map_err(|e| e.to_string())?;
        Ok(threads
            .into_iter()
            .map(|t| ArchiveThreadRecord {
                external_id: t.key,
                platform: t.platform,
                title: t.title,
                participants: t.participants,
                messages: t
                    .messages
                    .into_iter()
                    .map(|m| ArchiveMessageRecord {
                        external_id: m.external_id,
                        sender_name: m.sender_name,
                        is_owner: m.is_owner,
                        created_at_ms: m.created_at_ms,
                        text: m.text,
                    })
                    .collect(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::machine::ArchiveImportMachine;
    use crate::nest::fakes::{FakeNest, MemoryOpener};
    use crate::snapshot::ArchiveImportAction as Act;
    use fauna_archive::testing::fixture_zip;

    /// Imports the generated Facebook fixture through the machine's own run,
    /// so the model files are exactly what a real import writes.
    async fn imported_fixture() -> Arc<FakeNest> {
        let nest = Arc::new(FakeNest::new());
        let opener = MemoryOpener::default();
        opener.insert("/exports/facebook.zip", fixture_zip("facebook-json"));
        let m = ArchiveImportMachine::new(
            nest.clone(),
            Arc::new(opener),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        );
        m.hydrate().await.unwrap();
        m.dispatch(Act::SetArchivePath {
            value: "/exports/facebook.zip".into(),
        })
        .await
        .unwrap();
        for action in [Act::OpenArchive, Act::Next, Act::Next, Act::Start] {
            m.dispatch(action).await.unwrap();
        }
        m.run_import().await.unwrap();
        nest
    }

    #[tokio::test]
    async fn the_fixture_thread_reads_back_with_the_owner_on_its_own_side() {
        let nest = imported_fixture().await;
        let threads = read_imported_threads(nest.as_ref()).await.expect("read");
        assert_eq!(threads.len(), 1, "{threads:?}");
        let t = &threads[0];
        assert_eq!(t.platform, "facebook");
        assert!(t.key.starts_with("facebook:"), "{}", t.key);
        assert_eq!(
            t.participants,
            ["Friend One"],
            "the owner is not a participant of their own thread"
        );
        assert!(!t.messages.is_empty());
        assert!(
            t.messages
                .windows(2)
                .all(|w| w[0].created_at_ms <= w[1].created_at_ms)
        );
        assert!(t.messages.iter().any(|m| m.is_owner) && t.messages.iter().any(|m| !m.is_owner));
    }

    #[tokio::test]
    async fn a_reimport_folder_repeats_rather_than_adds() {
        let nest = imported_fixture().await;
        let once = read_imported_threads(nest.as_ref()).await.expect("read");
        // A second, newer folder of the same export: its model files repeat.
        let folders = nest.list_archive_folders().await.expect("folders");
        let first = &folders[0];
        let mut marker = first.marker.clone();
        marker.created_at = fauna_core::data::Timestamp(marker.created_at.0 + 1);
        nest.seed_folder("archive-again", &marker, &nest.read_state(&first.folder));
        for category in [Category::Threads, Category::Messages] {
            let bytes = nest
                .file(&first.folder, &model_path(&category))
                .expect("model");
            nest.put_file("archive-again", &model_path(&category), bytes);
        }
        assert_eq!(
            read_imported_threads(nest.as_ref()).await.expect("read"),
            once
        );
    }

    #[tokio::test]
    async fn no_archive_folder_lists_nothing() {
        let nest = FakeNest::new();
        assert!(read_imported_threads(&nest).await.expect("read").is_empty());
    }
}
