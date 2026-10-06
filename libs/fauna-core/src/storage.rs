//! Local content storage: posts, blobs, profiles, and indexes.

use std::path::Path;

use redb::{Database, TableDefinition};

use crate::data::{Post, PostId, Profile};
use crate::encoding::{canonical_decode, canonical_encode};
use crate::error::{Error, Result};
use crate::identity::ActorId;

const POSTS_TABLE: TableDefinition<&[u8; 36], &[u8]> = TableDefinition::new("posts");
const PROFILES_TABLE: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("profiles");

/// Minimal local content store backed by redb.
pub struct Store {
    db: Database,
}

impl Store {
    /// Open a store at the given file path, creating it if needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path).map_err(|e| Error::Storage(e.to_string()))?;
        let store = Self { db };
        store.ensure_tables()?;
        Ok(store)
    }

    /// Open an in-memory store (for tests).
    pub fn open_in_memory() -> Result<Self> {
        let db = Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(|e| Error::Storage(e.to_string()))?;
        let store = Self { db };
        store.ensure_tables()?;
        Ok(store)
    }

    fn ensure_tables(&self) -> Result<()> {
        let tx = self
            .db
            .begin_write()
            .map_err(|e| Error::Storage(e.to_string()))?;
        // Opening the tables creates them if they don't exist.
        let _ = tx
            .open_table(POSTS_TABLE)
            .map_err(|e| Error::Storage(e.to_string()))?;
        let _ = tx
            .open_table(PROFILES_TABLE)
            .map_err(|e| Error::Storage(e.to_string()))?;
        tx.commit().map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    /// Store a post by its PostId.
    pub fn put_post(&self, post_id: &PostId, post: &Post) -> Result<()> {
        let bytes = canonical_encode(post)?;
        let tx = self
            .db
            .begin_write()
            .map_err(|e| Error::Storage(e.to_string()))?;
        {
            let mut table = tx
                .open_table(POSTS_TABLE)
                .map_err(|e| Error::Storage(e.to_string()))?;
            table
                .insert(post_id.as_bytes(), bytes.as_slice())
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        tx.commit().map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    /// Retrieve a post by its PostId.
    pub fn get_post(&self, post_id: &PostId) -> Result<Option<Post>> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| Error::Storage(e.to_string()))?;
        let table = tx
            .open_table(POSTS_TABLE)
            .map_err(|e| Error::Storage(e.to_string()))?;
        match table
            .get(post_id.as_bytes())
            .map_err(|e| Error::Storage(e.to_string()))?
        {
            Some(value) => {
                let post: Post = canonical_decode(value.value())?;
                Ok(Some(post))
            }
            None => Ok(None),
        }
    }

    /// Check whether a post exists.
    pub fn has_post(&self, post_id: &PostId) -> Result<bool> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| Error::Storage(e.to_string()))?;
        let table = tx
            .open_table(POSTS_TABLE)
            .map_err(|e| Error::Storage(e.to_string()))?;
        let exists = table
            .get(post_id.as_bytes())
            .map_err(|e| Error::Storage(e.to_string()))?
            .is_some();
        Ok(exists)
    }

    /// Store a profile (keyed by actor_id).
    pub fn put_profile(&self, profile: &Profile) -> Result<()> {
        let bytes = canonical_encode(profile)?;
        let tx = self
            .db
            .begin_write()
            .map_err(|e| Error::Storage(e.to_string()))?;
        {
            let mut table = tx
                .open_table(PROFILES_TABLE)
                .map_err(|e| Error::Storage(e.to_string()))?;
            table
                .insert(&profile.actor_id.0, bytes.as_slice())
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        tx.commit().map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    /// Retrieve a profile by actor_id.
    pub fn get_profile(&self, actor_id: &ActorId) -> Result<Option<Profile>> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| Error::Storage(e.to_string()))?;
        let table = tx
            .open_table(PROFILES_TABLE)
            .map_err(|e| Error::Storage(e.to_string()))?;
        match table
            .get(&actor_id.0)
            .map_err(|e| Error::Storage(e.to_string()))?
        {
            Some(value) => {
                let profile: Profile = canonical_decode(value.value())?;
                Ok(Some(profile))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{InboxMode, PostBody, Timestamp};
    use crate::encoding::compute_post_id;
    use crate::identity::ActorKeypair;

    #[test]
    fn post_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let kp = ActorKeypair::generate();
        let post = Post {
            author: kp.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: "stored!".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        // Post identity is now its bare-encoded hash; the SignedEnvelope
        // ships alongside in the embed-as-bytes wire shape (callers store
        // it separately — this in-memory Store is the bytes-only side).
        let post_id = compute_post_id(&post).unwrap();

        store.put_post(&post_id, &post).unwrap();
        assert!(store.has_post(&post_id).unwrap());

        let loaded = store.get_post(&post_id).unwrap().unwrap();
        assert_eq!(loaded.author, post.author);
        assert_eq!(loaded.body, post.body);
    }

    #[test]
    fn profile_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let kp = ActorKeypair::generate();
        let profile = Profile {
            actor_id: kp.actor_id(),
            display_name: Some("Alice".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: InboxMode::Open,
            recovery_head: None,
            updated_at: Timestamp::now(),
        };

        store.put_profile(&profile).unwrap();
        let loaded = store.get_profile(&kp.actor_id()).unwrap().unwrap();
        assert_eq!(loaded.display_name, Some("Alice".into()));
    }
}
