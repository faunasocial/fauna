//! The folded archive comments' read side (`docs/goal/behavior/archive-import.md`
//! § What each category becomes → *Placement under the owner's own post*):
//! every comment in the owner's archives left on one of the owner's own posts,
//! placed under the post it was left on by ruling 5's ladder — the same
//! [`resolve`] phase two's merge runs — over the owner's own origin index.
//!
//! The population is every comment whose target owner is the owner, whoever
//! wrote it, minus the private copies a linked contact has claimed (they
//! arrive as that contact's own replies). A comment no rung places is
//! *unplaced*: under no post, only counted.
//!
//! Like [`crate::threads`], the read is plain over [`ArchiveNest`] and returns
//! crate-local records; with the `feed-sink` feature, [`ArchiveComments`] is
//! the feed manager's `ArchiveCommentSource` over it.

use std::collections::{BTreeMap, BTreeSet};

use fauna_archive::model::ArchiveComment;
use fauna_archive::{Category, Entity, ExternalId, Platform};
use fauna_core::data::Timestamp;

use crate::link::{LINKS_PATH, LinksState, OriginEntry, resolve, same_person};
use crate::nest::{ArchiveFolderRef, ArchiveNest, ArchiveNestError};
use crate::state::{ImportState, ImportTarget, STATE_PATH};
use crate::threads::read_model;

/// One comment as the fold shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoldedComment {
    /// `<platform>:<id>` — the comment's external id.
    pub key: String,
    /// The Fauna post id (hex) of the owner's imported post it sits under.
    pub post_id: String,
    pub author_name: String,
    pub created_at_ms: i64,
    pub text: String,
    pub media_count: u64,
}

/// Every placed comment, by the post it sits under (oldest first), and the
/// count of those no rung placed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldedComments {
    pub by_post: BTreeMap<String, Vec<FoldedComment>>,
    pub unplaced: u64,
}

impl FoldedComments {
    /// Every placed comment, flattened in post order.
    pub fn all(&self) -> impl Iterator<Item = &FoldedComment> {
        self.by_post.values().flatten()
    }
}

fn key_of(id: &ExternalId) -> String {
    format!("{}:{}", id.platform.token(), id.id)
}

fn micros_to_ms(micros: u64) -> i64 {
    i64::try_from(micros / 1000).unwrap_or(i64::MAX)
}

/// The fold across every platform the account imported from.
pub async fn read_folded_comments(
    nest: &dyn ArchiveNest,
) -> Result<FoldedComments, ArchiveNestError> {
    let mut folders = nest.list_archive_folders().await?;
    folders.retain(|f| f.marker.platform.is_known());
    folders.sort_by_key(|f| std::cmp::Reverse(f.marker.created_at));
    let mut platforms: Vec<Platform> = Vec::new();
    for folder in &folders {
        if !platforms.contains(&folder.marker.platform) {
            platforms.push(folder.marker.platform.clone());
        }
    }
    let mut out = FoldedComments::default();
    for platform in platforms {
        let of: Vec<ArchiveFolderRef> = folders
            .iter()
            .filter(|f| f.marker.platform == platform)
            .cloned()
            .collect();
        fold_platform(nest, &of, &mut out).await?;
    }
    for comments in out.by_post.values_mut() {
        comments.sort_by(|a, b| (a.created_at_ms, &a.key).cmp(&(b.created_at_ms, &b.key)));
    }
    Ok(out)
}

/// One platform's folders, newest first.
async fn fold_platform(
    nest: &dyn ArchiveNest,
    folders: &[ArchiveFolderRef],
    out: &mut FoldedComments,
) -> Result<(), ArchiveNestError> {
    let owner = &folders[0].marker.owner;
    let index = own_index(nest, folders).await?;
    let hidden = hidden_ids(nest, folders).await?;
    let mut seen = BTreeSet::new();
    for folder in folders {
        for entity in read_model(nest, &folder.folder, &Category::Comments).await? {
            let Entity::Comment(comment) = entity else {
                continue;
            };
            if !seen.insert(comment.external_id.clone())
                || hidden.contains(&comment.external_id)
                || !comment
                    .target
                    .owner
                    .as_ref()
                    .is_some_and(|o| same_person(o, owner))
            {
                continue;
            }
            match resolve(&comment.target, Timestamp(comment.created_at.0), &index).hit() {
                Some(target) => out
                    .by_post
                    .entry(target.post_id.clone())
                    .or_default()
                    .push(folded(&comment, &target.post_id)),
                None => out.unplaced += 1,
            }
        }
    }
    Ok(())
}

fn folded(comment: &ArchiveComment, post_id: &str) -> FoldedComment {
    FoldedComment {
        key: key_of(&comment.external_id),
        post_id: post_id.to_string(),
        author_name: comment.author.display_name.clone(),
        created_at_ms: micros_to_ms(comment.created_at.0),
        text: comment.text.clone().unwrap_or_default(),
        media_count: comment.media.len() as u64,
    }
}

/// The owner's own origin index: every imported post of the platform, each
/// external id once, with no audience filter — the owner reads their own.
async fn own_index(
    nest: &dyn ArchiveNest,
    folders: &[ArchiveFolderRef],
) -> Result<Vec<OriginEntry>, ArchiveNestError> {
    let mut posts = BTreeMap::new();
    for folder in folders {
        for entity in read_model(nest, &folder.folder, &Category::Posts).await? {
            if let Entity::Post(post) = entity {
                posts.entry(post.external_id.clone()).or_insert(post);
            }
        }
    }
    let mut out: Vec<OriginEntry> = Vec::new();
    for folder in folders {
        let Some(bytes) = nest.read_file(&folder.folder, STATE_PATH).await? else {
            continue;
        };
        let Ok(state) = fauna_cbor::decode_strict::<ImportState>(&bytes) else {
            continue;
        };
        for record in state.imported {
            let ImportTarget::Post { post_id } = record.target else {
                continue;
            };
            let Some(post) = posts.get(&record.external_id) else {
                continue;
            };
            if out.iter().any(|e| e.external_id == record.external_id) {
                continue;
            }
            out.push(OriginEntry {
                external_id: record.external_id,
                url: post.url.clone(),
                links: post.links.clone(),
                created_at: Timestamp(post.created_at.0),
                post_id,
                public: false,
            });
        }
    }
    Ok(out)
}

/// The private copies a linked contact has claimed, across the platform's
/// folders' link states (ruling 3's union).
async fn hidden_ids(
    nest: &dyn ArchiveNest,
    folders: &[ArchiveFolderRef],
) -> Result<BTreeSet<ExternalId>, ArchiveNestError> {
    let mut states = Vec::new();
    for folder in folders {
        if let Some(bytes) = nest.read_file(&folder.folder, LINKS_PATH).await?
            && let Ok(state) = fauna_cbor::decode_strict::<LinksState>(&bytes)
        {
            states.push(state);
        }
    }
    Ok(LinksState::union(states)
        .received
        .into_iter()
        .map(|r| r.item.external_id)
        .collect())
}

/// The feed manager's `ArchiveCommentSource` over the account's archive
/// folders.
#[cfg(feature = "feed-sink")]
pub struct ArchiveComments(pub std::sync::Arc<dyn ArchiveNest>);

/// Register `archive`'s folded comments as `manager`'s archive comment source
/// — the one call every face makes (tui and linux at login, the FFI feed
/// factory, the web core), the [`crate::threads::register_archive_threads`]
/// shape, so no face constructs [`ArchiveComments`] itself. Reads nothing:
/// the face re-reads through `FeedManager::refresh_archive_comments`.
#[cfg(feature = "feed-sink")]
pub fn register_archive_comments<R: fauna_protocol::RpcRequester + Clone>(
    manager: &fauna_feed::FeedManager<R>,
    archive: std::sync::Arc<dyn ArchiveNest>,
) {
    manager.set_archive_comment_source(std::sync::Arc::new(ArchiveComments(archive)));
}

#[cfg(feature = "feed-sink")]
impl fauna_feed::ArchiveCommentSource for ArchiveComments {
    fn archive_comments(&self) -> fauna_feed::ArchiveCommentsFuture<'_> {
        Box::pin(async move {
            let folded = read_folded_comments(self.0.as_ref())
                .await
                .map_err(|e| e.to_string())?;
            Ok(folded
                .all()
                .map(|c| fauna_feed::ArchiveCommentRecord {
                    post_id: c.post_id.clone(),
                    external_id: c.key.clone(),
                    author_name: c.author_name.clone(),
                    created_at_ms: c.created_at_ms,
                    text: c.text.clone(),
                    media_count: c.media_count,
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fauna_archive::model::{ArchivePost, ArchiveTargetRef};
    use fauna_archive::{ArchiveAudience, EntityKind, ExternalActorRef};

    use super::*;
    use crate::link::{ClaimedItem, ReceivedClaim};
    use crate::machine::ArchiveImportMachine;
    use crate::nest::fakes::{FakeNest, MemoryOpener};
    use crate::snapshot::ArchiveImportAction as Act;
    use crate::state::{
        ArchiveMarker, ImportPhase, ImportScope, ImportedRecord, StoredAudienceMode, model_path,
    };
    use fauna_archive::testing::fixture_zip;

    const FB: Platform = Platform::Facebook;
    const FOLDER: &str = "Facebook archive";
    const LINK: &str = "https://example.invalid/post/1";

    fn person(name: &str) -> ExternalActorRef {
        ExternalActorRef::new(FB, None, name)
    }

    fn ext(kind: EntityKind, id: &str) -> ExternalId {
        ExternalId {
            platform: FB,
            kind,
            id: id.to_string(),
        }
    }

    fn post_id_of(id: &str) -> String {
        blake3::hash(id.as_bytes()).to_hex().to_string()
    }

    fn post(id: &str, at: u64, links: &[&str]) -> ArchivePost {
        ArchivePost {
            external_id: ext(EntityKind::Post, id),
            created_at: fauna_archive::Timestamp(at),
            audience: ArchiveAudience::OnlyMe,
            text: Some(id.to_string()),
            media: vec![],
            links: links.iter().map(|l| l.to_string()).collect(),
            tagged: vec![],
            album_name: None,
            url: None,
            place: None,
        }
    }

    fn comment(id: &str, author: &str, owner: &str, url: Option<&str>, at: u64) -> Entity {
        Entity::Comment(ArchiveComment {
            external_id: ext(EntityKind::Comment, id),
            created_at: fauna_archive::Timestamp(at),
            author: person(author),
            target: ArchiveTargetRef {
                id: None,
                url: url.map(str::to_string),
                owner: Some(person(owner)),
                kind_hint: None,
            },
            text: Some(format!("words {id}")),
            media: vec![],
        })
    }

    /// One folder of Alice's: the posts, all imported, and the comments.
    fn archive(posts: Vec<ArchivePost>, comments: Vec<Entity>) -> FakeNest {
        let nest = FakeNest::new();
        let state = ImportState {
            import_id: "00".repeat(32),
            scope: ImportScope {
                categories: vec![Category::Posts],
                audience_mode: StoredAudienceMode::Original,
                date_from: None,
                date_until_exclusive: None,
                extra: Default::default(),
            },
            phase: ImportPhase::Finished,
            imported: posts
                .iter()
                .map(|p| ImportedRecord {
                    external_id: p.external_id.clone(),
                    target: ImportTarget::Post {
                        post_id: post_id_of(&p.external_id.id),
                    },
                })
                .collect(),
            skipped: vec![],
            updated_at: Timestamp(1),
            in_flight: None,
            extra: Default::default(),
        };
        let marker = ArchiveMarker {
            platform: FB,
            owner: person("Alice Archive"),
            import_id: "00".repeat(32),
            parser_version: 1,
            raw_file_name: "facebook.zip".into(),
            created_at: Timestamp(10),
            raw_len: None,
            raw_blake3: None,
        };
        nest.seed_folder(FOLDER, &marker, &state);
        let posts: Vec<Entity> = posts.into_iter().map(Entity::Post).collect();
        for (category, entities) in [(Category::Posts, posts), (Category::Comments, comments)] {
            nest.put_file(
                FOLDER,
                &model_path(&category),
                fauna_cbor::encode_canonical(&entities).unwrap(),
            );
        }
        nest
    }

    #[tokio::test]
    async fn a_comment_on_the_owners_link_post_folds_under_it_and_an_unknown_link_is_unplaced() {
        let nest = archive(
            vec![post("p-link", 100, &[LINK]), post("p-plain", 100, &[])],
            vec![
                comment("c-friend", "Friend Two", "Alice Archive", Some(LINK), 200),
                comment("c-own", "Alice Archive", "Alice Archive", Some(LINK), 300),
                comment(
                    "c-unknown",
                    "Friend Two",
                    "Alice Archive",
                    Some("https://x"),
                    200,
                ),
                comment("c-nourl", "Friend Two", "Alice Archive", None, 200),
                // On someone else's post: not in the fold's population at all.
                comment("c-theirs", "Alice Archive", "Friend One", Some(LINK), 200),
            ],
        );
        let folded = read_folded_comments(&nest).await.expect("read");
        let under = &folded.by_post[&post_id_of("p-link")];
        let got: Vec<(&str, &str)> = under
            .iter()
            .map(|c| (c.author_name.as_str(), c.key.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("Friend Two", "facebook:c-friend"),
                ("Alice Archive", "facebook:c-own")
            ],
            "whoever wrote it, oldest first"
        );
        assert_eq!(under[0].text, "words c-friend");
        assert_eq!(under[0].created_at_ms, 0, "micros to millis");
        assert_eq!(folded.by_post.len(), 1);
        assert_eq!(folded.unplaced, 2, "the unknown link and the url-less one");
    }

    #[tokio::test]
    async fn a_claimed_private_copy_is_left_out_of_the_fold() {
        let nest = archive(
            vec![post("p-link", 100, &[LINK])],
            vec![comment(
                "c-friend",
                "Friend Two",
                "Alice Archive",
                Some(LINK),
                200,
            )],
        );
        let links = LinksState {
            received: vec![ReceivedClaim {
                contact: "bb".repeat(32),
                platform: FB,
                item: ClaimedItem {
                    external_id: ext(EntityKind::Comment, "c-friend"),
                    post_id: "cc".repeat(32),
                    target_post_id: post_id_of("p-link"),
                },
                at: Timestamp(5),
            }],
            ..Default::default()
        };
        nest.put_file(
            FOLDER,
            LINKS_PATH,
            fauna_cbor::encode_canonical(&links).unwrap(),
        );
        let folded = read_folded_comments(&nest).await.expect("read");
        assert!(folded.by_post.is_empty(), "{folded:?}");
        assert_eq!(folded.unplaced, 0, "a hidden copy is not counted unplaced");
    }

    #[tokio::test]
    async fn two_link_posts_sharing_the_link_place_nothing() {
        let nest = archive(
            vec![post("a", 100, &[LINK]), post("b", 110, &[LINK])],
            vec![comment("c", "Friend Two", "Alice Archive", Some(LINK), 200)],
        );
        let folded = read_folded_comments(&nest).await.expect("read");
        assert!(folded.by_post.is_empty());
        assert_eq!(folded.unplaced, 1);
    }

    /// The generated fixture through the machine's own run: its one comment
    /// on the owner's post names a link no imported post shared before it,
    /// so it is unplaced; the owner's comment on a friend's photo is not in
    /// the population.
    #[tokio::test]
    async fn the_fixture_run_folds_nothing_and_counts_its_one_owner_targeted_comment() {
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
        let folded = read_folded_comments(nest.as_ref()).await.expect("read");
        assert!(folded.by_post.is_empty(), "{folded:?}");
        assert_eq!(folded.unplaced, 1);
        assert_eq!(m.snapshot().comments_unplaced, 1, "the Done step's line");
    }

    #[tokio::test]
    async fn no_archive_folder_folds_nothing() {
        let folded = read_folded_comments(&FakeNest::new()).await.expect("read");
        assert_eq!(folded, FoldedComments::default());
    }
}
