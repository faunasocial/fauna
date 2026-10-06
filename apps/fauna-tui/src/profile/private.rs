//! The profile's **private section** — the viewer's own nickname, notes and
//! labels on the person being viewed (`docs/goal/ui/profile.md` § The private
//! section owns this edit surface; `docs/goal/ui/contacts.md` § The private
//! overlay owns the record, the merge and where the nickname paints).
//!
//! OTHER only. Glue over shared Rust: the staging rule (lazy, diffed from the
//! form as the user opened it), the changed-register diff, the bounds
//! validation and its refusals are `fauna_core::contact_overlay`
//! (`OverlayEditor`); the Save is
//! `fauna_client_account_runtime::contact_overlays::save`, the one door every
//! app writes through. This file owns the text buffers and nothing else.

use fauna_core::contact_overlay::OverlayForm;
use fauna_i18n::strings::profile as t;
use fauna_sync_engine::contact_overlay_rows::{OverlayWrite, OverlayWriteOutcome};
use fauna_ui_ids as ids;

use super::{Action, Op, Outcome, ProfileField, ProfileState};
use crate::app::App;
use crate::element::{Element, Field, Gesture};
use crate::pages::Page;

impl ProfileState {
    /// The overlay form as the projection holds it right now.
    fn live_private_form(&self) -> OverlayForm {
        self.viewing
            .as_deref()
            .and_then(|actor| Some(self.overlays.as_ref()?.form(actor)))
            .unwrap_or_default()
    }

    /// What the private section shows: the staged edits once editing began,
    /// else the live projection.
    pub(super) fn private_form(&self) -> OverlayForm {
        self.private_edit.form(&self.live_private_form())
    }

    /// The viewer's nickname for the viewed person, when one is set — the
    /// header's primary line.
    pub(super) fn viewed_nickname(&self) -> Option<String> {
        let actor = self.viewing.as_deref()?;
        self.overlays.as_ref()?.nickname(actor)
    }
}

pub(super) fn field(state: &ProfileState, field: &ProfileField) -> Option<String> {
    Some(match field {
        ProfileField::PrivateNickname => state.private_form().nickname,
        ProfileField::PrivateNotes => state.private_form().notes,
        ProfileField::PrivateLabel => state.label_input.clone(),
        _ => return None,
    })
}

/// Stage one private-section buffer; `false` when `field` is not one of them.
pub(super) fn set_field(state: &mut ProfileState, field: &ProfileField, value: String) -> bool {
    match field {
        ProfileField::PrivateNickname => {
            let live = state.live_private_form();
            state.private_edit.set_nickname(&live, value);
        }
        ProfileField::PrivateNotes => {
            let live = state.live_private_form();
            state.private_edit.set_notes(&live, value);
        }
        ProfileField::PrivateLabel => state.label_input = value,
        _ => return false,
    }
    true
}

pub(super) fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    match action {
        Action::AddPrivateLabel => {
            let st = &mut app.profile;
            let live = st.live_private_form();
            match st.private_edit.add_label(&live, &st.label_input) {
                Err(refusal) => {
                    app.errors
                        .insert(Page::Profile, crate::wizard::localized(&refusal));
                }
                Ok(()) => {
                    st.label_input.clear();
                    app.errors.remove(&Page::Profile);
                }
            }
            None
        }
        Action::RemovePrivateLabel(i) => {
            let live = app.profile.live_private_form();
            app.profile.private_edit.remove_label(&live, i);
            None
        }
        Action::SavePrivate => {
            let st = &app.profile;
            let actor = st.viewing.clone()?;
            let changes = match st.private_edit.changes() {
                Ok(Some(changes)) => changes,
                // Nothing staged, or staged back to where it started: nothing
                // to write.
                Ok(None) => {
                    app.profile.private_edit.reset();
                    return None;
                }
                Err(refusal) => {
                    app.errors
                        .insert(Page::Profile, crate::wizard::localized(&refusal));
                    return None;
                }
            };
            let (Some(manager), Some(store)) = (
                app.conversations.manager.clone(),
                app.settings.account_store.clone(),
            ) else {
                // The account store assembles off the login path; until it has,
                // there is nowhere to write — the staged edits stay on screen.
                app.errors.insert(
                    Page::Profile,
                    fauna_i18n::strings::common::STILL_LOADING.to_string(),
                );
                return None;
            };
            Some(Op::SavePrivate {
                manager,
                store,
                actor,
                write: OverlayWrite::Changes(changes),
            })
        }
        _ => unreachable!("only the private-section actions are routed here"),
    }
}

/// `Op::SavePrivate`'s body.
pub(super) async fn save(
    manager: std::sync::Arc<fauna_conversations::ConversationsManager>,
    store: fauna_sync_engine::account_runtime::AccountStoreHandle,
    actor: String,
    write: OverlayWrite,
) -> Outcome {
    match fauna_client_account_runtime::contact_overlays::save(&manager, &store, &actor, write)
        .await
    {
        Ok(OverlayWriteOutcome::Written(_) | OverlayWriteOutcome::Unchanged(_)) => {
            Outcome::PrivateSaved { actor }
        }
        Ok(OverlayWriteOutcome::Refused(refusal)) => {
            Outcome::Failed(crate::wizard::localized(&refusal))
        }
        Err(e) => Outcome::Failed(
            match fauna_client_account_runtime::contact_overlays::not_ready_reason(&e) {
                // A replica that is not ready — it has never listed the
                // account's records, or holds them under a generation it may
                // still be keyed for — refuses the save at the read gate; it
                // can be retried once what it waits for has come, and the
                // page says what that is.
                Some(reason) => reason.to_string(),
                // The plane refuses to seal while no generation tip resolves
                // for this device (`account-data-taxonomy.md` → *The
                // contact-overlay rung*); the staged edits stay on screen
                // either way.
                None => t::PRIVATE_SAVE_FAILED.replace("{reason}", &format!("{e:#}")),
            },
        ),
    }
}

/// Fold a landed Save: the projection already reloaded, so drop the staging
/// and let the fields read live again — unless the page moved on to another
/// person meanwhile, whose staging is not this Save's to drop.
pub(super) fn apply_saved(app: &mut App, actor: &str) {
    if app.profile.viewing.as_deref() == Some(actor) {
        app.profile.private_edit.reset();
        app.errors.remove(&Page::Profile);
    }
}

/// The section's elements, painted on OTHER only (below the relationship
/// actions, above the tab strip).
pub(super) fn elements(st: &ProfileState, out: &mut Vec<Element>) {
    let form = st.private_form();
    out.push(Element::label(
        ids::PROFILE_PRIVATE_SECTION,
        t::PRIVATE_TITLE,
    ));
    out.push(
        Element::input(
            ids::PROFILE_NICKNAME_FIELD,
            form.nickname,
            Field::Profile(ProfileField::PrivateNickname),
        )
        .labelled(t::PRIVATE_NICKNAME),
    );
    out.push(
        Element::input(
            ids::PROFILE_NOTES_FIELD,
            form.notes,
            Field::Profile(ProfileField::PrivateNotes),
        )
        .labelled(t::PRIVATE_NOTES),
    );
    out.push(Element::label(ids::PROFILE_LABEL_LIST, t::PRIVATE_LABELS));
    for (i, label) in form.labels.iter().enumerate() {
        out.push(Element::label(ids::PROFILE_LABEL_CHIP, label.clone()));
        out.push(Element::gesture_button(
            ids::PROFILE_LABEL_REMOVE_BUTTON,
            t::PRIVATE_LABEL_REMOVE,
            true,
            Gesture::Profile(Action::RemovePrivateLabel(i)),
        ));
    }
    out.push(
        Element::input(
            ids::PROFILE_LABEL_FIELD,
            st.label_input.clone(),
            Field::Profile(ProfileField::PrivateLabel),
        )
        .labelled(t::PRIVATE_LABEL_ADD),
    );
    out.push(Element::gesture_button(
        ids::PROFILE_LABEL_ADD_BUTTON,
        t::PRIVATE_LABEL_ADD,
        true,
        Gesture::Profile(Action::AddPrivateLabel),
    ));
    out.push(Element::gesture_button(
        ids::PROFILE_PRIVATE_SAVE_BUTTON,
        t::PRIVATE_SAVE,
        true,
        Gesture::Profile(Action::SavePrivate),
    ));
}
