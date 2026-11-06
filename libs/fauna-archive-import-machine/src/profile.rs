//! Step 6's profile prefill (`docs/goal/behavior/archive-import.md` § What
//! each category becomes, the Profile row: "offered as a one-tap prefill at
//! Done, never applied silently"; § The wizard and its machine, step 6).
//!
//! Two halves, kept apart so the offer can never write: the run's end reads
//! the archive's profile out of the folder's `model/profile.cbor` onto the
//! snapshot ([`ArchiveImportMachine::offer_archive_profile`]), and only the
//! user's tap — [`crate::ArchiveImportAction::ApplyArchiveProfile`] — signs it
//! onto their own profile, through the same read-modify-write every profile
//! edit form runs (`fauna_client_profile::build_edited_profile_with_images`),
//! so the fields the archive does not carry are preserved.

use fauna_archive::model::ArchiveProfile;
use fauna_archive::{Category, Entity};
use fauna_client_profile::{ProfileImageEdit, build_edited_profile_with_images};
use fauna_core::data::ProfileLink;

use crate::machine::{ArchiveImportDispatchError, ArchiveImportMachine};
use crate::nest::{ArchiveNest, ArchiveNestError};
use crate::offload::off_runtime;
use crate::snapshot::ArchiveProfileView;
use crate::state::raw_path;
use crate::threads::read_model;

/// The archive's own profile, from `folder`'s model — `None` when the export
/// carried none (or its model file is absent or unreadable).
pub(crate) async fn read_archive_profile(
    nest: &dyn ArchiveNest,
    folder: &str,
) -> Result<Option<ArchiveProfile>, ArchiveNestError> {
    Ok(read_model(nest, folder, &Category::Profile)
        .await?
        .into_iter()
        .find_map(|entity| match entity {
            Entity::Profile(profile) => Some(profile),
            _ => None,
        }))
}

impl From<&ArchiveProfile> for ArchiveProfileView {
    fn from(p: &ArchiveProfile) -> Self {
        Self {
            display_name: p.actor.display_name.clone(),
            bio: p.bio.clone().filter(|b| !b.trim().is_empty()),
            links: p.links.clone(),
            has_picture: p.picture.is_some(),
        }
    }
}

/// A profile link's label: the address without its scheme or trailing slash
/// — the archive carries bare URLs, and the profile record wants a label
/// beside each.
fn link_of(uri: &str) -> ProfileLink {
    let label = uri
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    ProfileLink {
        label,
        uri: uri.to_string(),
    }
}

impl ArchiveImportMachine {
    /// The run finished: put the archive's profile on the Done step's offer.
    /// A failed read leaves the offer unmade (the button paints disabled),
    /// never fails the run.
    pub(crate) async fn offer_archive_profile(&self, folder: &str) {
        match read_archive_profile(self.nest.as_ref(), folder).await {
            Ok(profile) => {
                let mut snap = self.inner.lock().expect("snapshot mutex");
                snap.archive_profile = profile.as_ref().map(ArchiveProfileView::from);
                snap.profile_applied = false;
            }
            Err(e) => tracing::warn!(%folder, "archive import: profile offer: {e}"),
        }
    }

    /// `ApplyArchiveProfile` — the tap. Each field the archive carries
    /// replaces the user's; a field it lacks keeps what the profile holds.
    /// The picture is read from the archive (or the folder's raw copy) and
    /// uploaded as a public blob, the avatar's own shape (`media.md` §
    /// Encryption at rest).
    pub(crate) async fn apply_archive_profile(&self) -> Result<(), ArchiveImportDispatchError> {
        let folder = {
            let snap = self.inner.lock().expect("snapshot mutex");
            if snap.profile_applied {
                return Ok(());
            }
            snap.folder_name.clone()
        };
        let Some(folder) = folder else {
            return Err(ArchiveImportDispatchError::InvalidState(
                "no finished import to take a profile from".into(),
            ));
        };
        let Some(archive) = read_archive_profile(self.nest.as_ref(), &folder).await? else {
            return Err(ArchiveImportDispatchError::InvalidState(
                "the archive carries no profile".into(),
            ));
        };

        let base = self.nest.own_profile_base(&self.keypair).await?;
        let current = match base.body.as_deref() {
            Some(body) => Some(
                fauna_client_profile::decode_profile_display(body)
                    .map_err(|e| ArchiveImportDispatchError::Archive(e.to_string()))?,
            ),
            None => None,
        };
        let display_name = Some(archive.actor.display_name.clone())
            .filter(|n| !n.trim().is_empty())
            .or_else(|| current.as_ref().and_then(|c| c.display_name.clone()));
        let bio = archive
            .bio
            .clone()
            .filter(|b| !b.trim().is_empty())
            .or_else(|| current.as_ref().and_then(|c| c.bio.clone()));
        let links = if archive.links.is_empty() {
            current.map(|c| c.links).unwrap_or_default()
        } else {
            archive.links.iter().map(|l| link_of(l)).collect()
        };

        let avatar = match &archive.picture {
            Some(picture) => {
                let source = self.archive_source(&folder).await?;
                // Off the runtime's thread, as every read the run makes
                // (`crate::offload`): the source is the folder's own copy.
                let wanted = picture.path.clone();
                let (name, bytes) = off_runtime(move || crate::run::read_media(&source, &[wanted]))
                    .await?
                    .map_err(ArchiveImportDispatchError::Archive)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        ArchiveImportDispatchError::Archive("the profile picture is missing".into())
                    })?;
                let uploaded = self.nest.upload_public_media(&name, bytes).await?;
                ProfileImageEdit::set_from_hex(&hex::encode(uploaded.blob_hash))
                    .map_err(|e| ArchiveImportDispatchError::Archive(e.to_string()))?
            }
            None => ProfileImageEdit::Keep,
        };

        let signed = build_edited_profile_with_images(
            &self.keypair,
            base.body.as_deref(),
            base.last_direct_body.as_deref(),
            &base.proven,
            display_name,
            bio,
            links,
            avatar,
            ProfileImageEdit::Keep,
            None,
        )
        .map_err(|e| ArchiveImportDispatchError::Archive(e.to_string()))?;
        self.nest.set_profile(signed).await?;
        self.inner.lock().expect("snapshot mutex").profile_applied = true;
        Ok(())
    }

    /// The archive to read the picture from: the one still open, else the
    /// folder's raw copy.
    async fn archive_source(
        &self,
        folder: &str,
    ) -> Result<crate::nest::SharedSource, ArchiveImportDispatchError> {
        if let Some(source) = self.source.lock().expect("source mutex").clone() {
            return Ok(source);
        }
        let raw_name = self
            .run
            .lock()
            .expect("run mutex")
            .as_ref()
            .map(|r| r.marker.raw_file_name.clone())
            .ok_or_else(|| {
                ArchiveImportDispatchError::InvalidState("open the archive again".into())
            })?;
        self.nest
            .open_folder_archive(folder, &raw_path(&raw_name))
            .await?
            .ok_or_else(|| {
                ArchiveImportDispatchError::InvalidState("open the archive again".into())
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use fauna_archive::testing::fixture_zip;
    use fauna_client_profile::{
        ProfileImageEdit, build_edited_profile_with_images, decode_profile_display,
    };
    use fauna_core::data::ProfileLink;

    use crate::machine::ArchiveImportMachine;
    use crate::nest::ArchiveNest;
    use crate::nest::fakes::{FakeNest, MemoryOpener};
    use crate::snapshot::{ArchiveImportAction as Act, ArchiveImportStep, ArchiveProfileView};

    const ARCHIVE_PATH: &str = "/exports/archive.zip";

    fn keypair() -> fauna_core::identity::ActorKeypair {
        fauna_core::identity::ActorKeypair::from_secret([7u8; 32])
    }

    /// A finished import of the fixture `name` over `nest`, on the Done step.
    async fn done_over(nest: Arc<FakeNest>, name: &str) -> ArchiveImportMachine {
        let opener = MemoryOpener::default();
        opener.insert(ARCHIVE_PATH, fixture_zip(name));
        let m = ArchiveImportMachine::new(nest, Arc::new(opener), keypair());
        m.hydrate().await.unwrap();
        m.dispatch(Act::SetArchivePath {
            value: ARCHIVE_PATH.into(),
        })
        .await
        .unwrap();
        m.dispatch(Act::OpenArchive).await.unwrap();
        m.dispatch(Act::Next).await.unwrap();
        m.dispatch(Act::Next).await.unwrap();
        m.dispatch(Act::Start).await.unwrap();
        m.run_import().await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Done);
        m
    }

    /// Outcome 13's two halves: the offer is on the Done step and NOTHING is
    /// written until the tap; the tap writes name, bio, links and picture.
    #[tokio::test]
    async fn the_profile_is_offered_at_done_and_written_only_on_the_tap() {
        let nest = Arc::new(FakeNest::new());
        let m = done_over(nest.clone(), "instagram-json").await;
        let offer = m.snapshot().archive_profile.expect("the offer is made");
        assert_eq!(offer.display_name, "Test Owner");
        assert_eq!(
            offer.links,
            vec!["https://example.invalid/owner".to_string()]
        );
        assert!(offer.has_picture);
        assert!(!m.snapshot().profile_applied);
        assert_eq!(nest.profile(), None, "the run never writes the profile");
        let uploads_before = nest.media_uploads().len();

        m.dispatch(Act::ApplyArchiveProfile).await.unwrap();
        let shown = decode_profile_display(&nest.profile().expect("written on the tap")).unwrap();
        assert_eq!(shown.display_name.as_deref(), Some("Test Owner"));
        assert_eq!(shown.bio, offer.bio);
        assert_eq!(
            shown.links,
            vec![ProfileLink {
                label: "example.invalid/owner".into(),
                uri: "https://example.invalid/owner".into(),
            }]
        );
        assert!(
            shown.avatar.is_some(),
            "the archive's picture is the avatar"
        );
        let uploads = nest.media_uploads();
        assert_eq!(uploads.len(), uploads_before + 1);
        assert!(!uploads.last().unwrap().2, "an avatar is a public blob");
        assert!(m.snapshot().profile_applied);
    }

    /// A field the archive does not carry keeps what the profile holds: the
    /// Facebook export names no picture, so the stored avatar survives.
    #[tokio::test]
    async fn a_field_the_archive_lacks_is_kept() {
        let nest = Arc::new(FakeNest::new());
        let avatar = ProfileImageEdit::set_from_hex(&"ab".repeat(32)).unwrap();
        let before = build_edited_profile_with_images(
            &keypair(),
            None,
            None,
            &[],
            Some("Old name".into()),
            None,
            vec![],
            avatar,
            ProfileImageEdit::Keep,
            None,
        )
        .unwrap();
        nest.set_profile(before).await.unwrap();
        let old_avatar = decode_profile_display(&nest.profile().unwrap())
            .unwrap()
            .avatar;

        let m = done_over(nest.clone(), "facebook-json").await;
        assert!(matches!(
            m.snapshot().archive_profile,
            Some(ArchiveProfileView {
                has_picture: false,
                ..
            })
        ));
        let stored = decode_profile_display(&nest.profile().unwrap()).unwrap();
        assert_eq!(
            stored.display_name.as_deref(),
            Some("Old name"),
            "untouched before the tap"
        );

        m.dispatch(Act::ApplyArchiveProfile).await.unwrap();
        let after = decode_profile_display(&nest.profile().unwrap()).unwrap();
        assert_eq!(after.display_name.as_deref(), Some("Test Owner"));
        assert!(after.bio.is_some());
        assert_eq!(after.avatar, old_avatar);
    }
}
