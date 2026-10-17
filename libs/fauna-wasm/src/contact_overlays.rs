//! The wasm face of the **private contact overlay** — the viewer's own
//! nickname, notes and labels on a person (`docs/goal/ui/contacts.md` § The
//! private overlay; the edit surface is `docs/goal/ui/profile.md` § The
//! private section). The web twin of fauna-ffi's `contact_overlays.rs`, and
//! as thin: every read is the projection's (`fauna_conversations::
//! ContactsCache`), the staging is `fauna_core::contact_overlay::
//! OverlayEditor`, and the Save is the one shared answer
//! (`fauna_account_seams::contact_overlays::save_staged`), so the SPA never
//! resolves a person's name, joins a label line or words a refusal.
//!
//! - [`WasmContactOverlays`] — the reads, for the surfaces keyed on the person
//!   the overlay is about: roster row, knock sender, feed and subscription
//!   author, Profile header. **Member chips and message senders never read
//!   here** — their names come from the conversations snapshot, which applies
//!   the paint gate (§ The private overlay → *The paint gate*). Built over the
//!   tab's manager (`WasmConversationsManager.contactOverlays()`), or with
//!   none (`new WasmContactOverlays()`) in a tab that hosts no runtime: an
//!   empty projection, so every name is the public label.
//! - [`WasmOverlayEditor`] — the private section's staging for one person and
//!   its Save. A face with no manager refuses the Save
//!   (`account-client-lifecycle.md` § The client-side lifecycle → *Bounds,
//!   stated*).
//!
//! **Change detection:** no observer reaches JS; the SPA compares
//! [`WasmContactOverlays::revision`] each time it re-reads the conversations
//! snapshot.
//!
//! The overlay seam itself is registered on the manager at the account
//! store's ready edge (`crate::account_runtime`, through
//! `conversation_seams::wire_parts`), so the projection is fed in the tab that
//! hosts the runtime.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use fauna_account_seams::contact_overlays::{StagedSave, save_staged};
use fauna_conversations::ConversationsManager;
use fauna_conversations::contacts::ContactsCache;
use fauna_core::contact_overlay::{OverlayEditor, OverlayForm};
use fauna_core::localized::LocalizedText;
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

/// JS shape of `fauna_core::format::PeerLabel`: the one line every surface
/// shows, and the public name it replaced when a nickname supplied it
/// (`null` otherwise — the secondary line is then absent).
#[derive(Serialize)]
struct PeerLabelJs {
    primary: String,
    public: Option<String>,
}

/// JS shape of `OverlayForm`, the private section's fields.
#[derive(Serialize)]
struct OverlayFormJs {
    nickname: String,
    notes: String,
    labels: Vec<String>,
}

impl From<OverlayForm> for OverlayFormJs {
    fn from(f: OverlayForm) -> Self {
        Self {
            nickname: f.nickname,
            notes: f.notes,
            labels: f.labels,
        }
    }
}

/// JS shape of the Save's answer: `kind` is `saved` | `refused` | `notReady`
/// | `failed`; every arm but `saved` carries the finished `text` the SPA
/// resolves (`resolveLocalized`) onto `error-message`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SaveAnswerJs {
    kind: &'static str,
    text: Option<LocalizedText>,
}

impl From<StagedSave> for SaveAnswerJs {
    fn from(answer: StagedSave) -> Self {
        let (kind, text) = match answer {
            StagedSave::Saved => ("saved", None),
            StagedSave::Refused(t) => ("refused", Some(t)),
            StagedSave::NotReady(t) => ("notReady", Some(t)),
            StagedSave::Failed(t) => ("failed", Some(t)),
        };
        Self { kind, text }
    }
}

/// The projection's reads for one tab (module docs).
#[wasm_bindgen]
pub struct WasmContactOverlays {
    manager: Option<Arc<ConversationsManager>>,
    /// The projection read: the manager's, or an empty one with no manager.
    cache: Arc<ContactsCache>,
}

impl WasmContactOverlays {
    pub(crate) fn over(manager: Arc<ConversationsManager>) -> Self {
        Self {
            cache: manager.contacts(),
            manager: Some(manager),
        }
    }
}

#[wasm_bindgen]
impl WasmContactOverlays {
    /// The face of a tab that hosts no runtime: an empty projection (every
    /// name its public label) and a Save that is refused.
    #[wasm_bindgen(constructor)]
    pub fn without_runtime() -> Self {
        Self {
            manager: None,
            cache: ContactsCache::new(),
        }
    }

    /// Moves exactly when the projection's content does — compare it on each
    /// conversations re-read to know names need re-reading. A float: the
    /// counter never nears 2^53, and a `u64` would cross as a `bigint`.
    pub fn revision(&self) -> f64 {
        self.cache.revision() as f64
    }

    /// What the viewer calls `actor_id` (`{ primary, public }`) — the shared
    /// resolver over this projection's nickname. `display_name` / `handle`
    /// are whatever public name the surface holds (`null` where none).
    #[wasm_bindgen(js_name = peerLabel)]
    pub fn peer_label(
        &self,
        display_name: Option<String>,
        handle: Option<String>,
        actor_id: &str,
    ) -> Result<JsValue, JsValue> {
        let label = self
            .cache
            .peer_label(display_name.as_deref(), handle.as_deref(), actor_id);
        crate::rpc::to_js(&PeerLabelJs {
            primary: label.primary,
            public: label.public,
        })
    }

    /// The roster row's `contact-labels` line; `undefined` when the person
    /// carries no label (the element is then absent).
    #[wasm_bindgen(js_name = labelsLine)]
    pub fn labels_line(&self, actor_id: &str) -> Option<String> {
        self.cache.labels_line(actor_id)
    }

    /// The roster filter over handle, domain, actor id, nickname and labels
    /// (`contacts.md` § The private overlay → *Labels and the roster filter*;
    /// notes are deliberately not matched). Empty query matches all.
    #[wasm_bindgen(js_name = matchesFilter)]
    pub fn matches_filter(
        &self,
        query: &str,
        handle: Option<String>,
        domain: Option<String>,
        actor_id: &str,
    ) -> bool {
        self.cache
            .matches_filter(query, handle.as_deref(), domain.as_deref(), actor_id)
    }

    /// What a subscription row calls its creator (`author_id` raw bytes).
    #[wasm_bindgen(js_name = subscriptionAuthorLabel)]
    pub fn subscription_author_label(&self, handle: Option<String>, author_id: &[u8]) -> String {
        self.cache
            .subscription_author_label(handle.as_deref(), author_id)
    }

    /// The derived label vocabulary — every live label across every person.
    pub fn vocabulary(&self) -> Result<JsValue, JsValue> {
        crate::rpc::to_js(&self.cache.vocabulary())
    }

    /// The private section's staging for `actor_id`.
    pub fn editor(&self, actor_id: String) -> WasmOverlayEditor {
        WasmOverlayEditor {
            manager: self.manager.clone(),
            cache: Arc::clone(&self.cache),
            actor_id,
            editor: Rc::new(RefCell::new(OverlayEditor::default())),
        }
    }
}

/// The private section's staging for one person: lazy, diffed from the form
/// as the user opened it, so a Save writes only the registers this user
/// changed here.
#[wasm_bindgen]
pub struct WasmOverlayEditor {
    manager: Option<Arc<ConversationsManager>>,
    cache: Arc<ContactsCache>,
    actor_id: String,
    /// `Rc` so the Save's future can drop the staging once it is spent.
    editor: Rc<RefCell<OverlayEditor>>,
}

impl WasmOverlayEditor {
    fn live(&self) -> OverlayForm {
        self.cache.form(&self.actor_id)
    }
}

#[wasm_bindgen]
impl WasmOverlayEditor {
    /// What the section shows (`{ nickname, notes, labels }`): the staged
    /// edits once editing began, else the live projection — so a sibling
    /// device's edit re-paints an untouched section.
    pub fn form(&self) -> Result<JsValue, JsValue> {
        let form = self.editor.borrow().form(&self.live());
        crate::rpc::to_js(&OverlayFormJs::from(form))
    }

    /// Whether an edit is staged.
    #[wasm_bindgen(js_name = isStaged)]
    pub fn is_staged(&self) -> bool {
        self.editor.borrow().is_staged()
    }

    #[wasm_bindgen(js_name = setNickname)]
    pub fn set_nickname(&self, value: String) {
        let live = self.live();
        self.editor.borrow_mut().set_nickname(&live, value);
    }

    #[wasm_bindgen(js_name = setNotes)]
    pub fn set_notes(&self, value: String) {
        let live = self.live();
        self.editor.borrow_mut().set_notes(&live, value);
    }

    /// Stage the label typed into the add field: `undefined` when staged, else
    /// the refusal (a `LocalizedText`), which stages nothing.
    #[wasm_bindgen(js_name = addLabel)]
    pub fn add_label(&self, raw: &str) -> Result<JsValue, JsValue> {
        let live = self.live();
        match self.editor.borrow_mut().add_label(&live, raw) {
            Ok(()) => Ok(JsValue::UNDEFINED),
            Err(text) => crate::rpc::to_js(&text),
        }
    }

    /// Unstage the label shown at `index` (out of range: nothing).
    #[wasm_bindgen(js_name = removeLabel)]
    pub fn remove_label(&self, index: u32) {
        let live = self.live();
        self.editor.borrow_mut().remove_label(&live, index as usize);
    }

    /// Drop the staging, so the fields read the live form again.
    pub fn reset(&self) {
        self.editor.borrow_mut().reset();
    }

    /// One Save for everything staged, resolving to `{ kind, text }`. On
    /// `saved` the staging is dropped and the projection has already
    /// reloaded; on every other answer the staged edits stay.
    pub fn save(&self) -> js_sys::Promise {
        let staged = self.editor.borrow().changes();
        let manager = self.manager.clone();
        let actor = self.actor_id.clone();
        let editor = Rc::clone(&self.editor);
        future_to_promise(async move {
            let store = crate::account_runtime::handle();
            let answer = save_staged(manager.as_deref(), store.as_ref(), &actor, staged).await;
            if answer == StagedSave::Saved {
                editor.borrow_mut().reset();
            }
            crate::rpc::to_js(&SaveAnswerJs::from(answer))
        })
    }
}
