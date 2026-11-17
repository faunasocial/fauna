//! Media's first folder — the plain folder an upload makes on an account that
//! owns none (`docs/goal/ui/media.md` § Errors & edge cases → *No folder to
//! upload into*, rulings 1–4).
//!
//! The create is the ordinary [`FolderWizardMachine`](crate::FolderWizardMachine)
//! driven headlessly ([`crate::headless`], the photo-library preset's drive):
//! audience private (the create default), no device enrolled, no location, no
//! retention, WebDAV off — each a choice the person makes afterwards under
//! Settings → Folders. *When* to make it (only at an upload with no target,
//! only on a complete read) is the media machine's rule; this module only
//! makes it, behind [`fauna_core::folder_create::PlainFolderMaker`].

use std::sync::Arc;

use fauna_core::folder_create::PlainFolderMaker;

use crate::headless::{HeadlessCreate, HeadlessObserver, create_headless, find_named};
use crate::nest_api::{FolderApiError, FolderNestApi};

/// The first folder's name — a fixed name, like
/// [`crate::photo_library::PHOTO_LIBRARY_SET_NAME`]; defined once in the
/// neutral home the media machine reads it from too.
pub use fauna_core::folder_create::FIRST_FOLDER_NAME;

/// [`PlainFolderMaker`] over the ordinary wizard and a [`FolderNestApi`] —
/// the production seam every app's Media builder injects
/// (`MediaMachine::set_folder_maker`).
pub struct HeadlessFolderMaker {
    api: Arc<dyn FolderNestApi>,
}

impl HeadlessFolderMaker {
    pub fn new(api: Arc<dyn FolderNestApi>) -> Self {
        Self { api }
    }

    async fn make(&self, name: &str) -> Result<(), FolderApiError> {
        let created = create_headless(
            Arc::clone(&self.api),
            Arc::new(HeadlessObserver),
            HeadlessCreate {
                name: name.to_string(),
                retention: None,
                devices: Vec::new(),
                flags: fauna_protocol::folders::PlaceFlags::default_place(),
            },
        )
        .await;
        match created {
            Ok(_) => Ok(()),
            // A duplicate name is a hard conflict on the create (the wizard
            // folds its class into its own sentence): another device made it
            // first, and the list now carries it — the race resolves to the
            // one folder (ruling 4). Anything else stands as the reason.
            Err(e) => match find_named(&self.api, name).await {
                Ok(Some(_)) => Ok(()),
                _ => Err(e),
            },
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl PlainFolderMaker for HeadlessFolderMaker {
    async fn make_plain_folder(&self, name: &str) -> Result<(), String> {
        self.make(name).await.map_err(|e| e.detail().to_string())
    }
}
