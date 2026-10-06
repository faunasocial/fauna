//! The in-memory projection of the **private contact overlay** — actor id →
//! [`ContactOverlay`], the viewer's own nickname, notes and labels on each
//! person (`docs/goal/ui/contacts.md` § The private overlay).
//!
//! Fed by the app's session from the account store handle's
//! `contact_overlays()` read (and re-fed on its change nudges) — this crate
//! never touches the sync engine itself, which keeps `fauna-conversations`
//! engine-free. Snapshot builders consult it so apps receive finished names
//! ([`fauna_core::format::peer_display_label`] is the one resolver) and never
//! resolve a nickname themselves.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use fauna_core::contact_overlay::{ContactOverlay, OverlayForm, label_vocabulary};
use fauna_core::format::{PeerLabel, contact_matches_filter, peer_display_label};

#[derive(Default)]
pub struct ContactsCache {
    overlays: RwLock<BTreeMap<String, ContactOverlay>>,
    /// Bumped by every [`Self::replace`] that changed something.
    revision: AtomicU64,
}

impl ContactsCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Replace the whole projection with a fresh load (keys are lowercase
    /// actor ids; empty overlays are dropped — they read as no overlay).
    /// Whether anything changed.
    pub fn replace(&self, overlays: BTreeMap<String, ContactOverlay>) -> bool {
        let overlays: BTreeMap<_, _> = overlays
            .into_iter()
            .filter(|(_, o)| !o.is_empty())
            .map(|(k, o)| (k.to_ascii_lowercase(), o))
            .collect();
        let mut held = self.overlays.write().unwrap();
        if *held == overlays {
            return false;
        }
        *held = overlays;
        self.revision.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// A counter that moves exactly when the projection's content does — what
    /// a surface outside the conversations snapshot (roster row, knock sender,
    /// Profile) compares on a manager wake to know its names need re-reading,
    /// without re-painting on every message.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Every person holding a non-empty overlay (lowercase actor ids).
    pub fn people(&self) -> Vec<String> {
        self.overlays.read().unwrap().keys().cloned().collect()
    }

    /// The overlay on `actor_id_hex`, when there is a non-empty one.
    pub fn overlay(&self, actor_id_hex: &str) -> Option<ContactOverlay> {
        self.overlays
            .read()
            .unwrap()
            .get(&actor_id_hex.to_ascii_lowercase())
            .cloned()
    }

    /// The viewer's nickname for `actor_id_hex`, when one is set.
    pub fn nickname(&self, actor_id_hex: &str) -> Option<String> {
        self.overlays
            .read()
            .unwrap()
            .get(&actor_id_hex.to_ascii_lowercase())
            .and_then(|o| o.nickname().map(str::to_string))
    }

    /// The live labels on `actor_id_hex`, ordered by their folded form.
    pub fn labels(&self, actor_id_hex: &str) -> Vec<String> {
        self.overlays
            .read()
            .unwrap()
            .get(&actor_id_hex.to_ascii_lowercase())
            .map(|o| o.live_labels().into_iter().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// The viewer's overlay on `actor_id_hex` as the private section's editable
    /// form (all empty when there is none).
    pub fn form(&self, actor_id_hex: &str) -> OverlayForm {
        self.overlay(actor_id_hex)
            .map(|o| o.form())
            .unwrap_or_default()
    }

    /// What the viewer calls `actor_id_hex` — the one shared resolver
    /// ([`fauna_core::format::peer_display_label`]) over this projection's
    /// nickname, so no surface resolves a person's name itself. `display_name`
    /// and `handle` are whatever public name the surface holds for them
    /// (`None` where it holds none). **Only for a surface keyed on the id the
    /// overlay is about** — roster row, knock sender, Profile header, feed and
    /// subscription author; member chips and message senders take their names
    /// from the conversations snapshot, which applies the paint gate
    /// (`contacts.md` § The private overlay → *The paint gate*).
    pub fn peer_label(
        &self,
        display_name: Option<&str>,
        handle: Option<&str>,
        actor_id_hex: &str,
    ) -> PeerLabel {
        peer_display_label(
            self.nickname(actor_id_hex).as_deref(),
            display_name,
            handle,
            actor_id_hex,
        )
    }

    /// What a subscription row calls its creator: the viewer's nickname for
    /// them, else the shared chooser's answer — the handle the nest resolved,
    /// else the hex actor id (`value-formatting.md` § Subscription author
    /// label, § Peer display label).
    pub fn subscription_author_label(&self, handle: Option<&str>, author_id: &[u8]) -> String {
        let hex = fauna_core::format::hex_full(author_id);
        self.peer_label(
            Some(&fauna_core::format::author_display_label(handle, author_id)),
            handle,
            &hex,
        )
        .primary
    }

    /// The roster row's `contact-labels` line — the person's live labels on
    /// one line; `None` when there are none (the element is then absent).
    pub fn labels_line(&self, actor_id_hex: &str) -> Option<String> {
        let labels = self.labels(actor_id_hex);
        (!labels.is_empty()).then(|| labels.join(", "))
    }

    /// The roster filter with this projection's nickname and labels beside
    /// handle, domain and actor id ([`contact_matches_filter`]; notes are
    /// deliberately not matched).
    pub fn matches_filter(
        &self,
        query: &str,
        handle: Option<&str>,
        domain: Option<&str>,
        actor_id_hex: &str,
    ) -> bool {
        contact_matches_filter(
            query,
            handle,
            domain,
            actor_id_hex,
            self.nickname(actor_id_hex).as_deref(),
            &self.labels(actor_id_hex),
        )
    }

    /// The derived label vocabulary — every live label across every person.
    pub fn vocabulary(&self) -> Vec<String> {
        label_vocabulary(self.overlays.read().unwrap().values())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::contact_overlay::{Register, Stamp};

    fn with_nick(nick: Option<&str>) -> ContactOverlay {
        ContactOverlay {
            nickname: Register {
                stamp: Stamp::new(1, [1; 32]),
                value: nick.map(str::to_string),
            },
            ..Default::default()
        }
    }

    #[test]
    fn the_projection_reads_by_actor_id_case_insensitively_and_drops_empties() {
        let cache = ContactsCache::default();
        let id = "AB".repeat(32);
        let mut load = BTreeMap::new();
        load.insert(id.clone(), with_nick(Some("Mum")));
        load.insert("cd".repeat(32), with_nick(None));
        assert!(cache.replace(load.clone()));
        let loaded = cache.revision();
        assert!(!cache.replace(load), "an identical load changes nothing");
        assert_eq!(cache.revision(), loaded, "and moves no revision");
        assert_ne!(loaded, ContactsCache::default().revision());
        assert_eq!(cache.nickname(&id.to_lowercase()).as_deref(), Some("Mum"));
        assert_eq!(cache.nickname(&id).as_deref(), Some("Mum"));
        assert!(
            cache.overlay(&"cd".repeat(32)).is_none(),
            "empty reads as none"
        );
        assert!(cache.labels(&id).is_empty());
    }

    #[test]
    fn the_projection_answers_the_row_names_the_labels_line_and_the_filter() {
        let cache = ContactsCache::default();
        let mum = "ab".repeat(32);
        let stranger = "cd".repeat(32);
        let mut overlay = with_nick(Some("Mum"));
        for label in ["Family", "Book club"] {
            overlay.labels.insert(
                label.to_lowercase(),
                Register {
                    stamp: Stamp::new(1, [1; 32]),
                    value: Some(label.to_string()),
                },
            );
        }
        cache.replace(BTreeMap::from([(mum.clone(), overlay)]));

        // A nickname heads the row and the public name it replaced rides beside it.
        let named = cache.peer_label(None, Some("alice"), &mum);
        assert_eq!(
            (named.primary.as_str(), named.public.as_deref()),
            ("Mum", Some("alice"))
        );
        // No overlay: exactly the public chain, one line.
        let plain = cache.peer_label(None, Some("bob"), &stranger);
        assert_eq!((plain.primary.as_str(), plain.public), ("bob", None));

        assert_eq!(
            cache.labels_line(&mum).as_deref(),
            Some("Book club, Family")
        );
        assert_eq!(cache.labels_line(&stranger), None);

        assert!(cache.matches_filter("mum", Some("alice"), None, &mum));
        assert!(cache.matches_filter("book", Some("alice"), None, &mum));
        assert!(!cache.matches_filter("book", Some("bob"), None, &stranger));
        assert!(cache.matches_filter("bob", Some("bob"), None, &stranger));

        assert_eq!(cache.form(&mum).nickname, "Mum");

        // A subscription's creator: the nickname, else the chooser (handle,
        // else the full hex id).
        assert_eq!(
            cache.subscription_author_label(Some("alice"), &[0xab; 32]),
            "Mum"
        );
        assert_eq!(
            cache.subscription_author_label(Some("bob"), &[0xcd; 32]),
            "bob"
        );
        assert_eq!(cache.subscription_author_label(None, &[0xcd; 32]), stranger);
        assert_eq!(cache.form(&stranger), OverlayForm::default());
    }
}
