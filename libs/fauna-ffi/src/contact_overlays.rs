//! UniFFI face of the **private contact overlay** — the viewer's own
//! nickname, notes and labels on a person (`docs/goal/ui/contacts.md` § The
//! private overlay; the edit surface is `docs/goal/ui/profile.md` § The
//! private section).
//!
//! Everything here is a pass-through to what tui and linux call directly, so
//! no app on this boundary resolves a person's name, joins a label line or
//! re-derives the staging rule:
//!
//! - [`FfiContactOverlays`] — the projection's reads
//!   (`fauna_conversations::ContactsCache`), for the surfaces keyed on the
//!   person the overlay is about: roster row, knock sender, feed and
//!   subscription author, Profile header. **Member chips and message senders
//!   never read here** — their names come from the conversations snapshot,
//!   which applies the paint gate (§ The private overlay → *The paint gate*).
//! - [`FfiOverlayEditor`] — the private section's staging for one person
//!   (`fauna_core::contact_overlay::OverlayEditor`) and its Save
//!   (`fauna_client_account_runtime::contact_overlays::save`, the one door
//!   every app writes through).
//!
//! **Build the face over the manager the app reads from now, every time.** An
//! app may swap its manager at login (android does), and a face built before
//! the swap reads the old one's projection. Construction is cheap, and it is
//! also what registers the overlay seam for a manager no conversations session
//! serves (`crate::account_runtime::overlay_seam`).
//!
//! **Change detection:** the projection re-emits on the manager's own
//! observer when its content moves; compare [`FfiContactOverlays::revision`]
//! on each wake to re-read names without re-painting on every message.
//!
//! Gated behind its own `contact-overlays` feature (default-on, dropped from
//! the Go mail-bridge `--no-default-features` build — the `member-review`
//! shape): the bridge has no contacts surface.

use std::sync::{Arc, Mutex};

use fauna_conversations::ConversationsManager;
use fauna_core::contact_overlay::{OverlayEditor, OverlayForm};
use fauna_core::format::PeerLabel;
use fauna_core::localized::LocalizedText;
use fauna_sync_engine::contact_overlay_rows::{OverlayWrite, OverlayWriteOutcome};

/// The overlay projection of one conversations manager.
#[derive(uniffi::Object)]
pub struct FfiContactOverlays {
    manager: Arc<ConversationsManager>,
}

#[uniffi::export]
impl FfiContactOverlays {
    /// The face over `manager` — the one the app reads from **now** (module
    /// docs).
    #[uniffi::constructor]
    pub fn new(manager: Arc<ConversationsManager>) -> Arc<Self> {
        crate::account_runtime::overlay_seam::ensure(&manager);
        Arc::new(Self { manager })
    }

    /// A counter that moves exactly when the projection's content does.
    pub fn revision(&self) -> u64 {
        self.manager.contacts().revision()
    }

    /// What the viewer calls `actor_id` (hex): `primary` is the nickname when
    /// one is set, else the public name; `public` is the public name a
    /// nickname replaced, for the secondary line (`contact-public-name`,
    /// `profile-public-name`) — absent when there is no nickname.
    /// `display_name` and `handle` are whatever public name the surface holds
    /// (`None` where it holds none): roster row `(None, handle)`, knock sender
    /// `(None, None)`, Profile header `(display_name, None)`, feed author
    /// `(display_name, handle)`.
    pub fn peer_label(
        &self,
        display_name: Option<String>,
        handle: Option<String>,
        actor_id: String,
    ) -> PeerLabel {
        self.manager
            .contacts()
            .peer_label(display_name.as_deref(), handle.as_deref(), &actor_id)
    }

    /// The roster row's `contact-labels` line; `None` when the person carries
    /// no label (the element is then absent).
    pub fn labels_line(&self, actor_id: String) -> Option<String> {
        self.manager.contacts().labels_line(&actor_id)
    }

    /// Does a roster row match the `contacts-search-field` query — handle,
    /// domain and actor id, plus the viewer's nickname and labels for that
    /// person (notes are deliberately not matched).
    pub fn matches_filter(
        &self,
        query: String,
        handle: Option<String>,
        domain: Option<String>,
        actor_id: String,
    ) -> bool {
        self.manager.contacts().matches_filter(
            &query,
            handle.as_deref(),
            domain.as_deref(),
            &actor_id,
        )
    }

    /// What a subscription row calls its creator: the viewer's nickname for
    /// them, else the handle the nest resolved, else the hex actor id.
    pub fn subscription_author_label(&self, handle: Option<String>, author_id: Vec<u8>) -> String {
        self.manager
            .contacts()
            .subscription_author_label(handle.as_deref(), &author_id)
    }

    /// The derived label vocabulary — every live label across every person.
    pub fn vocabulary(&self) -> Vec<String> {
        self.manager.contacts().vocabulary()
    }

    /// The private section's editor for `actor_id` (hex) — one per Profile
    /// open, dropped when the page moves to another person.
    pub fn editor(&self, actor_id: String) -> Arc<FfiOverlayEditor> {
        Arc::new(FfiOverlayEditor {
            manager: Arc::clone(&self.manager),
            actor_id,
            editor: Mutex::new(OverlayEditor::default()),
        })
    }
}

/// What a Save did. Every text is a finished `LocalizedText`: the app
/// resolves it and shows it on the page's `error-message`, with no wording of
/// its own.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum FfiOverlaySave {
    /// Written, or nothing to write. The staging is dropped: the fields read
    /// the live projection again.
    Saved,
    /// A bounds refusal or a label cap. The staged edits stay on screen.
    Refused { text: LocalizedText },
    /// Nothing can be written yet, and a retry will work once what the text
    /// names has come: the account store is still assembling
    /// (`common.still_loading`), this device has not listed the account's
    /// records from its nest (`common.needs_nest`), or it holds them under a
    /// key only another of the account's devices can hand over
    /// (`common.needs_other_device`). The staged edits stay on screen.
    NotReady { text: LocalizedText },
    /// The write failed (`profile.private_save_failed`, the cause as its
    /// `reason`). The staged edits stay on screen.
    Failed { text: LocalizedText },
}

/// The private section's staging for one person: lazy, diffed from the form
/// as the user opened it, so a Save writes only the registers this user
/// changed here.
#[derive(uniffi::Object)]
pub struct FfiOverlayEditor {
    manager: Arc<ConversationsManager>,
    actor_id: String,
    editor: Mutex<OverlayEditor>,
}

impl FfiOverlayEditor {
    fn live(&self) -> OverlayForm {
        self.manager.contacts().form(&self.actor_id)
    }

    fn staging(&self) -> std::sync::MutexGuard<'_, OverlayEditor> {
        self.editor.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[uniffi::export]
impl FfiOverlayEditor {
    /// What the section shows: the staged edits once editing began, else the
    /// live projection — so a sibling device's edit re-paints an untouched
    /// section. Re-read it when [`FfiContactOverlays::revision`] moves.
    pub fn form(&self) -> OverlayForm {
        self.staging().form(&self.live())
    }

    /// Whether an edit is staged (the fields no longer follow the live form).
    pub fn is_staged(&self) -> bool {
        self.staging().is_staged()
    }

    pub fn set_nickname(&self, value: String) {
        let live = self.live();
        self.staging().set_nickname(&live, value);
    }

    pub fn set_notes(&self, value: String) {
        let live = self.live();
        self.staging().set_notes(&live, value);
    }

    /// Stage the label typed into the add field. `None` when it was staged;
    /// a refusal (shown on `error-message`) stages nothing.
    pub fn add_label(&self, raw: String) -> Option<LocalizedText> {
        let live = self.live();
        self.staging().add_label(&live, &raw).err()
    }

    /// Unstage the label shown at `index` (out of range: nothing).
    pub fn remove_label(&self, index: u32) {
        let live = self.live();
        self.staging().remove_label(&live, index as usize);
    }

    /// Drop the staging, so the fields read the live form again.
    pub fn reset(&self) {
        self.staging().reset();
    }
}

#[fauna_uniffi_async::export]
impl FfiOverlayEditor {
    /// One Save for everything staged: only the registers this user changed
    /// are written, and the projection reloads before this returns, so the
    /// saving device re-paints at once.
    pub async fn save(&self) -> FfiOverlaySave {
        // Bound first: a guard taken in the `match` scrutinee would live for
        // the whole match, and the `Ok(None)` arm locks the staging again.
        let staged = self.staging().changes();
        let changes = match staged {
            Ok(Some(changes)) => changes,
            // Nothing staged, or staged back to where it started.
            Ok(None) => {
                self.staging().reset();
                return FfiOverlaySave::Saved;
            }
            Err(text) => return FfiOverlaySave::Refused { text },
        };
        // The account store assembles off the login path; until it has,
        // there is nowhere to write.
        let Some(store) = crate::account_runtime::handle() else {
            return FfiOverlaySave::NotReady {
                text: LocalizedText::key("common.still_loading"),
            };
        };
        let saved = fauna_client_account_runtime::contact_overlays::save(
            &self.manager,
            &store,
            &self.actor_id,
            OverlayWrite::Changes(changes),
        )
        .await;
        match saved {
            Ok(OverlayWriteOutcome::Written(_) | OverlayWriteOutcome::Unchanged(_)) => {
                self.staging().reset();
                FfiOverlaySave::Saved
            }
            Ok(OverlayWriteOutcome::Refused(text)) => FfiOverlaySave::Refused { text },
            Err(e) => failed(&e),
        }
    }
}

/// A Save error as the app shows it: the read gate's refusal names what the
/// save waits for; anything else is the save failure with its cause.
fn failed(e: &anyhow::Error) -> FfiOverlaySave {
    match fauna_client_account_runtime::contact_overlays::not_ready_text(e) {
        Some(text) => FfiOverlaySave::NotReady { text },
        None => FfiOverlaySave::Failed {
            text: LocalizedText::key_arg("profile.private_save_failed", "reason", format!("{e:#}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};

    use super::*;

    fn register(value: &str) -> Register {
        Register {
            stamp: Stamp::new(1, [1; 32]),
            value: Some(value.to_string()),
        }
    }

    /// A manager whose projection holds "Mum", labelled Family, on `ab…`.
    fn a_manager() -> (Arc<ConversationsManager>, String) {
        let manager = ConversationsManager::new();
        let mum = "ab".repeat(32);
        let mut overlay = ContactOverlay {
            nickname: register("Mum"),
            ..Default::default()
        };
        overlay
            .labels
            .insert("family".to_string(), register("Family"));
        assert!(manager.apply_contact_overlays(
            manager.contact_overlays_generation(),
            BTreeMap::from([(mum.clone(), overlay)]),
        ));
        (manager, mum)
    }

    /// The face without its seam registration — a store another test
    /// installed must not load its (empty) overlays over this projection.
    fn face(manager: Arc<ConversationsManager>) -> FfiContactOverlays {
        FfiContactOverlays { manager }
    }

    /// The face answers exactly what the projection answers — a mutation of
    /// any pass-through must not survive.
    #[test]
    fn the_reads_agree_with_the_projection() {
        let (manager, mum) = a_manager();
        let face = face(Arc::clone(&manager));
        let cache = manager.contacts();

        assert_eq!(face.revision(), cache.revision());
        let named = face.peer_label(None, Some("alice".into()), mum.clone());
        assert_eq!(
            (named.primary.as_str(), named.public.as_deref()),
            ("Mum", Some("alice"))
        );
        let stranger = "cd".repeat(32);
        let plain = face.peer_label(None, Some("bob".into()), stranger.clone());
        assert_eq!((plain.primary.as_str(), plain.public), ("bob", None));

        assert_eq!(face.labels_line(mum.clone()).as_deref(), Some("Family"));
        assert_eq!(face.labels_line(stranger.clone()), None);
        assert!(face.matches_filter("mum".into(), Some("alice".into()), None, mum.clone()));
        assert!(face.matches_filter("fam".into(), Some("alice".into()), None, mum.clone()));
        assert!(!face.matches_filter("fam".into(), Some("bob".into()), None, stranger));
        assert_eq!(
            face.subscription_author_label(Some("alice".into()), vec![0xab; 32]),
            "Mum"
        );
        assert_eq!(face.vocabulary(), ["Family"]);
    }

    /// The editor follows the live form until the first edit, stages from
    /// there, refuses an empty label without staging it, and reads live again
    /// once reset.
    #[test]
    fn the_editor_stages_over_the_live_form() {
        let (manager, mum) = a_manager();
        let editor = face(manager).editor(mum);

        assert!(!editor.is_staged());
        assert_eq!(editor.form().nickname, "Mum");
        assert_eq!(editor.form().labels, ["Family"]);

        let refusal = editor.add_label("   ".into());
        assert!(refusal.is_some(), "an empty label is refused");
        assert!(!editor.is_staged(), "and a refusal stages nothing");

        assert_eq!(editor.add_label(" Book   club ".into()), None);
        editor.set_nickname("Mother".into());
        editor.set_notes("allergic to cats".into());
        let staged = editor.form();
        assert_eq!(staged.nickname, "Mother");
        assert_eq!(staged.notes, "allergic to cats");
        assert_eq!(staged.labels, ["Family", "Book club"]);

        editor.remove_label(0);
        assert_eq!(editor.form().labels, ["Book club"]);
        editor.remove_label(9);
        assert_eq!(editor.form().labels, ["Book club"]);

        editor.reset();
        assert!(!editor.is_staged());
        assert_eq!(editor.form().nickname, "Mum");
    }

    /// A Save with nothing to write — nothing staged, or staged back to where
    /// it started — answers `Saved` and drops the staging. It used to never
    /// answer: the staging guard taken in the `match` scrutinee was still
    /// held when the arm locked the staging again to reset it.
    #[tokio::test]
    async fn a_save_with_nothing_to_write_answers_saved() {
        let (manager, mum) = a_manager();
        let editor = face(manager).editor(mum);

        assert_eq!(editor.save().await, FfiOverlaySave::Saved);

        editor.set_nickname("Mother".into());
        editor.set_nickname("Mum".into());
        assert!(editor.is_staged());
        assert_eq!(editor.save().await, FfiOverlaySave::Saved);
        assert!(!editor.is_staged(), "the staging is dropped");
    }

    /// A failure that is not the read gate's refusal is the save failure
    /// with its cause — the read gate's own two reasons are pinned beside
    /// `fauna_account_seams::contact_overlays::not_ready_text`.
    #[test]
    fn a_plain_save_error_is_the_save_failure_with_its_cause() {
        assert_eq!(
            failed(&anyhow::anyhow!("no generation tip")),
            FfiOverlaySave::Failed {
                text: LocalizedText::key_arg(
                    "profile.private_save_failed",
                    "reason",
                    "no generation tip"
                )
            }
        );
    }
}
