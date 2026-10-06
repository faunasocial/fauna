//! The **private contact overlay** on linux — the viewer's own nickname, notes
//! and labels on a person (`docs/goal/ui/contacts.md` § The private overlay).
//!
//! The projection is the shared `fauna_conversations::ContactsCache` the
//! conversations manager holds, fed from the account store by the shared
//! `contact_overlays` seam. Message bubbles and member chips take their names
//! from the conversations snapshot (which applies the paint gate); the
//! surfaces keyed on the person the overlay is *about* — roster row, knock
//! sender, feed author, Profile — read the projection through this module and
//! re-read it on [`watch`].

use std::sync::Arc;

use fauna_conversations::contacts::ContactsCache;
use gtk::prelude::*;

/// This process's overlay projection.
pub fn projection() -> Arc<ContactsCache> {
    super::manager().contacts()
}

/// Run `on_change` on the GTK main thread whenever the projection's content
/// has moved, for as long as `anchor` stays in a window. Call it **before**
/// the surface's first paint, so a load landing between the two is not missed.
///
/// The manager wakes its observers on every snapshot change; the projection's
/// revision is what keeps a message arriving from re-painting a roster. The
/// loop ends itself once `anchor` has been rooted and then loses its window
/// (the same guard `views::conversations` holds, for the same reason: the
/// window is rebuilt on every session change and a page per open).
pub fn watch(anchor: &impl IsA<gtk::Widget>, mut on_change: impl FnMut() + 'static) {
    let manager = super::manager();
    let rx = super::observer::attach(&manager);
    let cache = manager.contacts();
    let anchor = anchor.clone().upcast::<gtk::Widget>();
    let mut seen = cache.revision();
    let mut was_rooted = false;
    crate::async_helper::spawn_wake_loop(rx, move || {
        if anchor.root().is_some() {
            was_rooted = true;
        } else if was_rooted {
            return glib::ControlFlow::Break;
        }
        let now = cache.revision();
        if now != seen {
            seen = now;
            on_change();
        }
        glib::ControlFlow::Continue
    });
}
