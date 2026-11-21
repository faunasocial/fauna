//! The seam a page outside the folder wizard uses to make a plain folder of
//! the caller's own — today Media's first folder (`docs/goal/ui/media.md`
//! § Errors & edge cases → *No folder to upload into*, ruling 3).
//!
//! It lives here, in the neutral home, for the same reason
//! [`crate::folder_keys::FolderKeyResolver`] does: the consumer
//! (`fauna-media-machine`) cannot depend on the producer
//! (`fauna-folders-machine`) — the folders machine's WS-RPC seam pulls
//! `fauna-client-folders`, whose `mls` feature already depends on the media
//! machine, so the edge would close a cycle. The production impl is
//! `fauna_folders_machine::first_folder::HeadlessFolderMaker`: the ordinary
//! `FolderWizardMachine` driven headlessly, never a second create path.

/// The first folder's name (`media.md` ruling 3) — a fixed name written once
/// as data, not a localized string, as `Photo Library` and the calendar's
/// `Personal` are: it rests sealed and is read by the person's every device,
/// whatever its language. Here, in the neutral home, so the media machine
/// (which decides when to make it) and the folders machine (which makes it)
/// name the one constant.
pub const FIRST_FOLDER_NAME: &str = "Personal";

/// Make one plain folder of the caller's own, by name, through the ordinary
/// create path: audience private (the create default), no device enrolled,
/// no location, no retention, WebDAV off.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait PlainFolderMaker: crate::MaybeSendSync {
    /// `Ok` when the nest holds an own folder named `name` after the call —
    /// made now, or made by another device first (a duplicate name is a hard
    /// conflict on the create, so the race resolves to the one folder,
    /// `media.md` ruling 4). `Err` carries the create's own reason, unlocalized
    /// (the caller wraps it in its page's sentence).
    async fn make_plain_folder(&self, name: &str) -> Result<(), String>;
}
