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

use crate::address::TypedAddress;

/// One edge of the account's contact roster, as `fauna.contacts.list` answers
/// it (`docs/goal/ui/contacts.md` § State & data shape) — what the recipient
/// picker's completion offers beside correspondents
/// (`docs/goal/ui/conversations.md` § Where logic lives → *Recipient
/// suggestions*). Fed by the session that holds `ContactsClient`; this crate
/// never reads the nest itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterEntry {
    /// Lowercase hex actor id.
    pub actor_id_hex: String,
    pub handle: Option<String>,
    pub domain: Option<String>,
    /// The edge's status token (`accepted` / `confirmed` / `blocked` / …).
    pub status: String,
}

#[derive(Default)]
pub struct ContactsCache {
    overlays: RwLock<BTreeMap<String, ContactOverlay>>,
    roster: RwLock<Vec<RosterEntry>>,
    /// Bumped by every [`Self::replace`] or [`Self::replace_roster`] that
    /// changed something.
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
    /// (`None` where it holds none); a surface holding no handle gets the
    /// contact roster's ([`Self::roster_handle`]), so a contact the account
    /// knows by handle never falls to the short id (`value-formatting.md`
    /// § Peer display label). **Only for a surface keyed on the id the
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
        let roster_handle = match handle {
            Some(_) => None,
            None => self.roster_handle(actor_id_hex),
        };
        peer_display_label(
            self.nickname(actor_id_hex).as_deref(),
            display_name,
            handle.or(roster_handle.as_deref()),
            actor_id_hex,
        )
    }

    /// The handle the contact roster holds for `actor_id_hex` — bare, as the
    /// roster row paints it — when it holds a non-empty one.
    pub fn roster_handle(&self, actor_id_hex: &str) -> Option<String> {
        self.roster
            .read()
            .unwrap()
            .iter()
            .find(|e| e.actor_id_hex.eq_ignore_ascii_case(actor_id_hex))
            .and_then(|e| e.handle.clone())
            .filter(|h| !h.trim().is_empty())
    }

    /// The roster's Fauna address for `actor_id_hex`, spelled as a handled
    /// participant displays (`handle@domain` when the nest knows both, else
    /// the bare handle); `None` when the roster holds no handle for them. What
    /// the conversations snapshot names a handle-less Fauna sender, member or
    /// thread label by.
    pub fn roster_address(&self, actor_id_hex: &str) -> Option<TypedAddress> {
        let roster = self.roster.read().unwrap();
        let e = roster
            .iter()
            .find(|e| e.actor_id_hex.eq_ignore_ascii_case(actor_id_hex))?;
        let actor_id = fauna_core::identity::ActorId::from_hex(&e.actor_id_hex).ok()?;
        let handle = roster_spelling(e)?;
        Some(TypedAddress::Fauna { handle, actor_id })
    }

    /// What the viewer calls `actor_id_hex` when this projection knows them —
    /// by a nickname or a roster handle — else `None`, so the surface keeps
    /// the name it already holds (a fauna-native notification row's
    /// `{sender}`, the nest's 8-hex prefix: `behavior/notifications.md`
    /// § Localized body).
    pub fn known_peer_label(&self, actor_id_hex: &str) -> Option<String> {
        if self.nickname(actor_id_hex).is_none() && self.roster_handle(actor_id_hex).is_none() {
            return None;
        }
        Some(self.peer_label(None, None, actor_id_hex).primary)
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

    /// Replace the contact roster with a fresh `fauna.contacts.list` load
    /// (ids lowercased). Whether anything changed.
    pub fn replace_roster(&self, roster: Vec<RosterEntry>) -> bool {
        let roster: Vec<_> = roster
            .into_iter()
            .map(|e| RosterEntry {
                actor_id_hex: e.actor_id_hex.to_ascii_lowercase(),
                ..e
            })
            .collect();
        let mut held = self.roster.write().unwrap();
        if *held == roster {
            return false;
        }
        *held = roster;
        self.revision.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Whether the roster holds a blocked edge to `actor_id_hex`.
    pub fn is_blocked(&self, actor_id_hex: &str) -> bool {
        self.roster
            .read()
            .unwrap()
            .iter()
            .any(|e| e.status == "blocked" && e.actor_id_hex.eq_ignore_ascii_case(actor_id_hex))
    }

    /// The roster's people whom `query` completes to, for the recipient
    /// picker: [`Self::matches_filter`] over handle, domain, actor id, the
    /// viewer's nickname and labels, plus the address spelling. Only accepted
    /// and confirmed edges (a blocked or pending one is nobody to write to),
    /// confirmed first, each in roster order; an empty query matches nobody
    /// (the picker is a completion, not a browse list). Each person comes back
    /// as their Fauna address (`handle@domain` when the nest knows a handle,
    /// else no handle) and the label the roster row paints
    /// ([`Self::peer_label`]).
    pub fn people_matching(&self, query: &str) -> Vec<(TypedAddress, String)> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let roster = self.roster.read().unwrap().clone();
        let mut out = Vec::new();
        for wanted in ["confirmed", "accepted"] {
            for e in roster.iter().filter(|e| e.status == wanted) {
                let Ok(actor_id) = fauna_core::identity::ActorId::from_hex(&e.actor_id_hex) else {
                    continue;
                };
                let handle = roster_spelling(e).unwrap_or_default();
                let address = TypedAddress::Fauna { handle, actor_id };
                let spelled = address.display();
                if !self.matches_filter(
                    q,
                    e.handle.as_deref(),
                    e.domain.as_deref(),
                    &e.actor_id_hex,
                ) && !spelled.to_lowercase().contains(&q.to_lowercase())
                {
                    continue;
                }
                let label = self
                    .peer_label(None, address.person_handle(), &e.actor_id_hex)
                    .primary;
                out.push((address, label));
            }
        }
        out
    }
}

/// A roster edge's Fauna handle spelling: `handle@domain` when the nest knows
/// both, else the bare handle; `None` with no handle.
fn roster_spelling(e: &RosterEntry) -> Option<String> {
    match (e.handle.as_deref(), e.domain.as_deref()) {
        (Some(h), Some(d)) if !h.is_empty() && !d.is_empty() => Some(format!("{h}@{d}")),
        (Some(h), _) if !h.is_empty() => Some(h.to_string()),
        _ => None,
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

    /// A contact the roster knows by handle is named by it wherever a surface
    /// holds no handle of its own (the OTHER Profile header, the knock row) —
    /// never by the short id (`value-formatting.md` § Peer display label).
    #[test]
    fn a_roster_contact_is_named_by_its_handle_where_the_surface_holds_none() {
        let cache = ContactsCache::default();
        let tester = "ec".repeat(32);
        let stranger = "cd".repeat(32);
        cache.replace_roster(vec![RosterEntry {
            actor_id_hex: tester.to_uppercase(),
            handle: Some("tester".into()),
            domain: Some("nest.test".into()),
            status: "accepted".into(),
        }]);

        assert_eq!(cache.peer_label(None, None, &tester).primary, "tester");
        assert_eq!(cache.roster_handle(&tester).as_deref(), Some("tester"));
        let spelled = cache.roster_address(&tester).map(|a| a.display());
        assert_eq!(spelled, Some(["tester", "nest.test"].join("@")));
        // The surface's own handle and display name still win.
        assert_eq!(
            cache.peer_label(None, Some("other"), &tester).primary,
            "other"
        );
        assert_eq!(
            cache.peer_label(Some("Tess"), None, &tester).primary,
            "Tess"
        );
        // Unknown to the roster: the short id, and no known label.
        assert_eq!(
            cache.peer_label(None, None, &stranger).primary,
            fauna_core::format::short_id(&stranger)
        );
        assert_eq!(cache.roster_address(&stranger), None);
        assert_eq!(cache.known_peer_label(&stranger), None);
        assert_eq!(cache.known_peer_label(&tester).as_deref(), Some("tester"));
        // A nickname alone makes the person known.
        cache.replace(BTreeMap::from([(stranger.clone(), with_nick(Some("Mum")))]));
        assert_eq!(cache.known_peer_label(&stranger).as_deref(), Some("Mum"));
    }
}
