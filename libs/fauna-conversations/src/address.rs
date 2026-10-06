//! Address and rail types for cross-protocol thread participants.

use fauna_core::identity::ActorId;
use fauna_core::source_glyph::{BridgeIdentitySnapshot, SourceGlyph};
use serde::{Deserialize, Serialize};

// ActorId is from fauna-core (a foreign crate). UniFFI's `remote` option
// restricts the impl to `crate::UniFfiTag`, satisfying the orphan rule.
#[cfg(feature = "uniffi")]
uniffi::custom_type!(ActorId, Vec<u8>, {
    remote,
    lower: |id| id.0.to_vec(),
    try_lift: |b| {
        let arr: [u8; 32] = b.try_into().map_err(|_| {
            uniffi::deps::anyhow::anyhow!("ActorId must be exactly 32 bytes")
        })?;
        Ok(ActorId(arr))
    },
});

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum Rail {
    FaunaMls,
    Smtp,
    /// Every bridge — one variant for all of them (`ui/conversations.md`
    /// § Where logic lives → *The `Bridged` adapter*, ruling 2). A **unit**
    /// variant: `Rail` stays `Copy` and backend registration stays keyed per
    /// rail, so the per-bridge identity (id, label, glyph) rides the thread —
    /// [`crate::snapshot::ThreadSummary::bridge`] — and the address —
    /// [`TypedAddress::Bridged`] — never the rail.
    Bridged,
}

impl Rail {
    /// The canonical icon concept for this rail — the conversations-side half
    /// of the shared `Rail`/`SourceKind → SourceGlyph` mapping that lets every
    /// app keep a single native-asset map across the rail and the feed
    /// badge. See [`fauna_core::source_glyph::SourceGlyph`] and
    /// `docs/goal/architecture/render-model.md` § Deltas → D5. The match is
    /// exhaustive so a new rail must pick its concept here, not silently in
    /// each app.
    ///
    /// On [`Rail::Bridged`] this is the generic bridge only — the fallback for
    /// a thread read with no bridge identity; a bridged thread's snapshot
    /// `glyph` is the one its bridge declared.
    pub fn glyph(&self) -> SourceGlyph {
        match self {
            Rail::FaunaMls => SourceGlyph::Fox,
            Rail::Smtp => SourceGlyph::Envelope,
            Rail::Bridged => SourceGlyph::Bridge,
        }
    }

    /// The canonical name form used on the wire (test-agent commands) and by
    /// native apps that need a plain string — the counterpart to [`Self::parse`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Rail::FaunaMls => "FaunaMls",
            Rail::Smtp => "Smtp",
            Rail::Bridged => "Bridged",
        }
    }

    /// The wire kind a send on this rail issues — what an app declares to the
    /// offline gate before offering the send, and `None` for a rail with no
    /// registered send backend (a third thing from "local" and from "needs a
    /// nest": the send resolves to `BackendError::NotSupported` with no call
    /// attempted).
    ///
    /// Shared for the same reason [`Self::glyph`] is: the match is exhaustive,
    /// so a new rail must answer this here rather than silently in each app.
    /// It is a protocol fact, not an app's choice — a copy that drifts either
    /// gates a send that cannot happen or offers one the gate never saw. Both
    /// composers on both apps were hand-holding an identical copy until
    /// 2026-08-23, and linux's carried a comment saying it was "transcribed
    /// verbatim from tui's ... so the two composers cannot disagree with tui"
    /// — which is the hazard written down, not removed.
    pub fn send_wire_kind(&self) -> Option<&'static str> {
        match self {
            Rail::FaunaMls => Some("fauna.conversations.channel.send"),
            Rail::Smtp => Some("fauna.email.send"),
            // Every bridge sends through the family's one user-side kind
            // (`architecture/apps/bridges.md` § Bridge-kind catalogue →
            // Phase G), registered nest-side and in the shared offline-class
            // table since 2026-10-03.
            Rail::Bridged => Some("fauna.bridges.conversation.send"),
        }
    }

    /// Parses [`Self::as_str`]'s output back into a `Rail`. Infallible by
    /// design — every call site this replaced (tui, linux) silently fell
    /// back to `Rail::FaunaMls` on unrecognized input, so that behavior is
    /// preserved here rather than introducing a new error type those
    /// call sites would just discard anyway.
    pub fn parse(s: &str) -> Rail {
        match s {
            "Smtp" => Rail::Smtp,
            "Bridged" => Rail::Bridged,
            _ => Rail::FaunaMls,
        }
    }
}

/// FFI-exported twin of [`Rail::glyph`] for native apps (windows / macos /
/// ios) that need the icon concept for an ad-hoc [`Rail`] with no precomputed
/// snapshot `glyph` — e.g. the recipient-picker suggestion, which derives a
/// rail from a typed address rather than a thread. Thread rows read
/// `ThreadSummary` / `ThreadDetail::glyph` directly; this is the same mapping
/// for the off-snapshot case, keeping the concept in Rust (D5). linux calls
/// `Rail::glyph` directly as a crate dep.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn rail_glyph(rail: Rail) -> SourceGlyph {
    rail.glyph()
}

/// FFI-exported twin of [`Rail::as_str`] — the counterpart to [`rail_parse`]
/// for native apps (windows / macos / ios / android) that currently
/// hand-duplicate this name mapping rather than depending on the crate
/// directly. linux and tui call [`Rail::as_str`] / [`Rail::parse`] directly.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn rail_as_str(rail: Rail) -> String {
    rail.as_str().to_string()
}

/// FFI-exported twin of [`Rail::parse`]. See [`rail_as_str`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn rail_parse(s: String) -> Rail {
    Rail::parse(&s)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum TypedAddress {
    Fauna {
        handle: String,
        actor_id: ActorId,
    },
    Email {
        email_address: String,
    },
    /// An address on a far network a bridge serves — `bridge_id` is the
    /// bridge's manifest id, `address` the far network's own spelling of the
    /// participant (`ui/conversations.md` § Where logic lives → *The `Bridged`
    /// adapter*, ruling 2 (c)). Never parsed in an app: the bridge's address
    /// grammar is matched nest-side only (ruling 2 (d)).
    Bridged {
        bridge_id: String,
        address: String,
    },
    /// An address of a kind a newer build writes and this one does not name —
    /// a participant or sender of a rail this build lacks, read out of a
    /// history slice or drafts blob another device wrote. Carried whole as
    /// its canonical bytes, so the slice this build merges and re-uploads
    /// keeps it byte-for-byte (`transport.md` § Schema and forward-compat
    /// discipline → *Rule 3 in full*; the shape:
    /// [`fauna_core::carried::canonical_bytes`]).
    ///
    /// It is shown, never acted on: it has no rail ([`Self::rail`] is `None`),
    /// so nothing sends to it, resolves it, or dials it; it names no person;
    /// it displays as a neutral placeholder. No build writes one except by
    /// passing a carried value through.
    #[serde(untagged, with = "fauna_core::carried::canonical_bytes")]
    Unknown {
        canonical: Vec<u8>,
    },
}

impl TypedAddress {
    /// The rail this address lives on — `None` for an
    /// [`Unknown`](Self::Unknown) address, which no rail of this build can
    /// reach (the restrictive reading: a caller that needs a rail to act
    /// refuses rather than guess one).
    pub fn rail(&self) -> Option<Rail> {
        match self {
            TypedAddress::Fauna { .. } => Some(Rail::FaunaMls),
            TypedAddress::Email { .. } => Some(Rail::Smtp),
            TypedAddress::Bridged { .. } => Some(Rail::Bridged),
            TypedAddress::Unknown { .. } => None,
        }
    }

    /// The person this address is *about* — `Some` on the Fauna rail, `None` on
    /// every rail that carries no actor id.
    ///
    /// One home for a mapping three surfaces were writing themselves: the
    /// thread-list's per-participant actor-id column
    /// ([`crate::state_json`]), tui's member-chip review join, and — from
    /// 2026-08-21 — web's. It is deliberately **not** a filter: the review
    /// roster's people are MLS group members a succession's sweep could vouch
    /// for nothing about, so "does this row have an actor id" and "could this
    /// row ever be flagged" are the same question, and answering it in one
    /// place is what stops a sibling surface from hand-rolling a
    /// `matches!(addr, TypedAddress::Fauna { .. })` that drifts.
    pub fn person_actor_id(&self) -> Option<ActorId> {
        match self {
            TypedAddress::Fauna { actor_id, .. } => Some(*actor_id),
            _ => None,
        }
    }

    /// The inverse of [`Self::rail`]: build an address on `rail` from one raw
    /// string, **before** anything has resolved it.
    ///
    /// ⚠ **Unresolved is in the name because the placeholder is real.** The
    /// Fauna arm carries a zeroed [`ActorId`] (nothing has looked the handle up
    /// yet). That is correct for a recipient the user has only typed, and
    /// wrong for anything that then treats the field as identity — resolve
    /// before comparing, sealing, or addressing.
    ///
    /// **`None` on [`Rail::Bridged`]**, the one rail a bare string cannot name:
    /// a bridged address is a bridge id *and* a far-network spelling. Its twin
    /// [`Self::unresolved_bridged`] takes both; a caller holding a rail and a
    /// raw string routes `Bridged` there, and the `Option` makes forgetting to
    /// a compile error rather than a sentinel to trip over later.
    ///
    /// It exists because the test agents' `conversations_inject_*` commands
    /// take `(rail, raw)` off the wire (convention 11 makes that command table
    /// a cross-app contract), and tui and linux each held a byte-identical
    /// private copy of this match — beside the `parse_rail` pair that became
    /// [`Rail::parse`]. Same fix, same reason.
    pub fn unresolved_for_rail(rail: Rail, raw: &str) -> Option<TypedAddress> {
        match rail {
            Rail::FaunaMls => Some(TypedAddress::Fauna {
                handle: raw.to_string(),
                actor_id: ActorId([0u8; 32]),
            }),
            Rail::Smtp => Some(TypedAddress::Email {
                email_address: raw.to_string(),
            }),
            Rail::Bridged => None,
        }
    }

    /// The [`Rail::Bridged`] twin of [`Self::unresolved_for_rail`]: an address
    /// on the bridge `bridge_id` serves, spelled `raw` as the far network spells
    /// it — what the test agents' `conversations_inject_*` commands build when
    /// they name a `bridge_id` beside the rail (`ui/conversations.md` § Where
    /// logic lives → *The `Bridged` adapter*, ruling 2 (c)). Unresolved for
    /// the same reason: nothing has asked the nest whether the bridge's grammar
    /// admits `raw`.
    pub fn unresolved_bridged(bridge_id: &str, raw: &str) -> TypedAddress {
        TypedAddress::Bridged {
            bridge_id: bridge_id.to_string(),
            address: raw.to_string(),
        }
    }

    /// Address-identity comparison for dedup / minus-self / reply-recipient
    /// removal. Email is compared ASCII-case-insensitively (SMTP addresses are
    /// case-insensitive in the domain, and in these client flows the local-part
    /// is too — the same normalization `SmtpBackend::send` uses to drop self
    /// from the envelope). Every other rail compares by its canonical fields
    /// (derived `PartialEq`).
    pub fn same_address(&self, other: &TypedAddress) -> bool {
        match (self, other) {
            (
                TypedAddress::Email { email_address: a },
                TypedAddress::Email { email_address: b },
            ) => a.eq_ignore_ascii_case(b),
            _ => self == other,
        }
    }

    /// Same-participant comparison — the predicate every **roster** edit uses.
    ///
    /// Differs from [`Self::same_address`] on exactly one rail, deliberately:
    /// two `Fauna` addresses are the same participant when their **actor ids**
    /// agree, whatever handles they wear. A handle is not an identity — that is
    /// the premise the whole unattested-member review rests on
    /// (`identity-succession.md` § Propagation), and it has to hold on the way
    /// *out* of a roster as well as on the way in: a Fauna roster read off MLS
    /// carries **empty** handles by construction (a leaf credential has no
    /// handle to give) and attacker-chosen ones by threat model, so a roster
    /// edit keyed on the handle either misses its target or takes bystanders
    /// with it.
    ///
    /// Every other rail has no identity beneath its address, so this defers to
    /// [`Self::same_address`] — including its ASCII-case-insensitive email
    /// comparison.
    pub fn same_participant(&self, other: &TypedAddress) -> bool {
        match (self, other) {
            (TypedAddress::Fauna { actor_id: a, .. }, TypedAddress::Fauna { actor_id: b, .. }) => {
                a == b
            }
            _ => self.same_address(other),
        }
    }

    /// The handle this address wears, when it has one — `Some` only on the
    /// Fauna rail, and **never `Some("")`**.
    ///
    /// The empty-is-absence half is the load-bearing one: a Fauna row seated off
    /// an MLS roster carries `handle: String::new()`, so an empty handle is the
    /// *ordinary* state of any member this device met through a Welcome or an
    /// inbound roster rather than by typing them. A caller asking for a name
    /// wants `None` there, not a name-shaped empty string
    /// (`value-formatting.md` § Account display label: the handle counts "when
    /// present and non-empty").
    ///
    /// Pairs with [`Self::person_actor_id`], and is the read half of what
    /// [`Self::display`] falls back *from*: a caller asking "is there a name
    /// here at all" must use this, because `display()` now always answers with
    /// something.
    pub fn person_handle(&self) -> Option<&str> {
        match self {
            TypedAddress::Fauna { handle, .. } if !handle.is_empty() => Some(handle),
            _ => None,
        }
    }

    /// Canonical user-facing display string for this address.
    ///
    /// The Fauna arm resolves through
    /// [`fauna_core::format::account_display_label`] — the handle when it has
    /// one, else the `short_id` of the actor id — because a Fauna address
    /// seated off an MLS roster carries **no handle at all**:
    /// `backends::fauna_mls::ingest_welcome` and
    /// [`crate::ConversationsManager::apply_inbound_roster`] both seat the
    /// members the MLS engine roster names, and that roster carries actor ids
    /// and nothing else (`conversation-rooms.md` § Implementation status today).
    /// Handing the empty handle back verbatim rendered such a member as a
    /// **blank participant row** on all seven apps.
    ///
    /// The fallback is not a new rule: it is the one the folders "Shared with"
    /// roster row already uses for its own unset-handle case
    /// (`value-formatting.md` § Account display label, fourth consumer — where
    /// the drift it replaced was a raw 64-hex blob on three apps), so a nameless
    /// member reads here exactly as it does there.
    ///
    /// ⚠ **Display only — and no longer user-typeable on the Fauna arm.** An
    /// elided id is not an address: compare with [`Self::same_participant`]
    /// (actor id), never with this string.
    /// [`Self::display`], with a bridged address's bridge named beside it by
    /// the label the bridge declared — so the user sees which far network the
    /// nest resolved it to (`ui/conversations.md` § Where logic lives → *The
    /// `Bridged` adapter*, ruling 2 (d)). `bridges` is the snapshot's
    /// `ConversationsSnapshot::bridges`; a bridge it does not list reads as
    /// the bare address.
    pub fn display_with_bridges(&self, bridges: &[BridgeIdentitySnapshot]) -> String {
        match self {
            Self::Bridged { bridge_id, .. } => bridges
                .iter()
                .find(|b| b.id == *bridge_id)
                .map(|b| format!("{} · {}", self.display(), b.label))
                .unwrap_or_else(|| self.display()),
            _ => self.display(),
        }
    }

    pub fn display(&self) -> String {
        match self {
            TypedAddress::Fauna { handle, actor_id } => {
                fauna_core::format::account_display_label(Some(handle), &actor_id.to_hex())
            }
            TypedAddress::Email { email_address } => email_address.clone(),
            // The far network's own spelling; the bridge is named by the
            // thread's glyph and label, never repeated on every participant.
            TypedAddress::Bridged { address, .. } => address.clone(),
            // A kind this build cannot read has no field to show: a neutral,
            // language-free placeholder, never a guessed name.
            TypedAddress::Unknown { .. } => UNKNOWN_ADDRESS_DISPLAY.to_string(),
        }
    }
}

/// What [`TypedAddress::display`] shows for an [`TypedAddress::Unknown`]
/// address — language-free, so no app needs a string for it.
pub const UNKNOWN_ADDRESS_DISPLAY: &str = "?";

/// Format-only synchronous parse of a user-typed string into a
/// [`TypedAddress`]. Mirrors the previous C# `ParseTypedAddress` helper —
/// no rail probing, just the prefix/shape recognizers the recipient
/// picker needs to drive its `resolve_state` while the user types.
///
/// Cannot produce [`TypedAddress::Fauna`]: that variant requires an
/// `ActorId` (32-byte public key) which only a real fauna_mls backend
/// probe can supply. A typed Fauna handle parses here as Email until
/// the backend resolves it asynchronously.
///
/// Cannot produce [`TypedAddress::Bridged`] either: which bridge a far-network
/// spelling belongs to is the nest's answer, matched against each bridge's
/// declared grammar (`ui/conversations.md` § Where logic lives → *The
/// `Bridged` adapter*, ruling 2 (d)), so no app parses one. A `did:`, a
/// two-`@` Fediverse handle or an `npub1…` — the shapes the retired Bluesky,
/// ActivityPub and Nostr rails claimed here — is therefore not recognised,
/// and neither is read as an email address.
///
/// FFI-exported so the native apps (windows/macos/ios/android) consume this
/// one recognizer for the `dm-reply-recipient-add` field + recipient picker
/// instead of each maintaining a per-app duplicate (priority #2/#4); linux
/// calls it directly as a Rust crate dep.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn try_parse_typed_address(raw: &str) -> Option<TypedAddress> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("did:") {
        return None;
    }
    if trimmed.matches('@').count() > 1 {
        return None;
    }
    if trimmed.contains('@') {
        return Some(TypedAddress::Email {
            email_address: trimmed.to_string(),
        });
    }
    None
}

/// Canonical user-facing display string for a [`TypedAddress`] — the FFI-exported
/// twin of [`TypedAddress::display`].
///
/// FFI-exported so the native apps (windows/macos/ios/android) consume this
/// one switch for sender/participant/recipient-chip display instead of each
/// maintaining a per-app variant→string duplicate (priority #2/#4); linux
/// calls [`TypedAddress::display`] directly as a Rust crate dep. Use this
/// whenever a raw `TypedAddress` must be rendered and no pre-computed display
/// string is at hand (e.g. `MessageSnapshot.sender_display` empty, or live
/// recipient-picker chips the user is still typing).
///
/// Behind the `client-display` feature (enabled by `fauna-ffi`'s default-on
/// `conversations-session`) so it is dropped from the Go mail-bridge's
/// `--no-default-features` FFI build — the bridge is a server with no address-
/// display surface, and an added export would otherwise force a Go-binding regen
/// the Windows dev machine can't run (no Go). Mirrors `fauna-ffi`'s
/// `markdown-authoring` gate on `wrap_markdown_selection`.
#[cfg(feature = "client-display")]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn typed_address_display(addr: &TypedAddress) -> String {
    addr.display()
}

/// FFI-exported twin of [`TypedAddress::display_with_bridges`] — a recipient
/// picker's chip or suggestion text.
#[cfg(feature = "client-display")]
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn typed_address_display_with_bridges(
    addr: &TypedAddress,
    bridges: Vec<BridgeIdentitySnapshot>,
) -> String {
    addr.display_with_bridges(&bridges)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bridged chip names its bridge by the label the bridge declared; a
    /// bridge the snapshot does not list, and every other rail, read bare.
    #[test]
    fn display_with_bridges_names_a_known_bridge_only() {
        let bridges = vec![BridgeIdentitySnapshot {
            id: "matrix".into(),
            label: "Matrix".into(),
            glyph: SourceGlyph::Globe,
        }];
        let bridged = |bridge_id: &str| TypedAddress::Bridged {
            bridge_id: bridge_id.into(),
            address: "@bob:example.org".into(),
        };
        assert_eq!(
            bridged("matrix").display_with_bridges(&bridges),
            format!("{} · Matrix", bridged("matrix").display())
        );
        assert_eq!(
            bridged("signal").display_with_bridges(&bridges),
            bridged("signal").display()
        );
        let email = TypedAddress::Email {
            email_address: "a@example.test".into(),
        };
        assert_eq!(email.display_with_bridges(&bridges), email.display());
    }

    /// **The blank participant row, pinned shut.** A Fauna member seated off an
    /// MLS roster carries `handle: String::new()` - the ordinary state of any
    /// member met through a Welcome or an inbound roster - and returning that
    /// verbatim rendered the row as nothing at all on all seven apps. The
    /// fallback is `value-formatting.md` § Account display label's ratified
    /// rule, not a new one.
    #[test]
    fn a_handle_less_fauna_address_displays_as_its_elided_actor_id() {
        let actor = ActorId([0xabu8; 32]);
        let nameless = TypedAddress::Fauna {
            handle: String::new(),
            actor_id: actor,
        };
        assert_eq!(
            nameless.display(),
            fauna_core::format::short_id(&actor.to_hex())
        );
        assert!(
            !nameless.display().is_empty(),
            "a member is never rendered as a blank row"
        );

        // A handle, when there is one, still wins outright.
        assert_eq!(
            TypedAddress::Fauna {
                handle: "alice@nest".into(),
                actor_id: actor,
            }
            .display(),
            "alice@nest"
        );
    }

    /// Two distinct nameless members must not read as the same person. This is
    /// what makes the fallback safe for the `display()`-keyed dedup in
    /// `backend::bucket_inbound_common`, which an empty-string display collapsed.
    #[test]
    fn two_handle_less_members_do_not_share_a_display() {
        let a = TypedAddress::Fauna {
            handle: String::new(),
            actor_id: ActorId([1u8; 32]),
        };
        let b = TypedAddress::Fauna {
            handle: String::new(),
            actor_id: ActorId([2u8; 32]),
        };
        assert_ne!(a.display(), b.display());
    }

    /// `person_handle` is the read half `display()` falls back from: it answers
    /// "is there a name here at all", so an empty handle is absence and every
    /// non-Fauna rail is `None` (their address *is* their name - there is no
    /// separate handle beneath it).
    #[test]
    fn person_handle_treats_an_empty_handle_as_absence() {
        assert_eq!(
            TypedAddress::Fauna {
                handle: "alice@nest".into(),
                actor_id: ActorId([1u8; 32]),
            }
            .person_handle(),
            Some("alice@nest")
        );
        assert_eq!(
            TypedAddress::Fauna {
                handle: String::new(),
                actor_id: ActorId([1u8; 32]),
            }
            .person_handle(),
            None,
            "an empty handle is absence, not a name"
        );
        assert_eq!(
            TypedAddress::Email {
                email_address: "bob@example.com".into(),
            }
            .person_handle(),
            None,
        );
    }

    #[test]
    fn rail_glyph_is_canonical_concept() {
        // The brand decision ratified by the user 2026-06-22 (render-model.md
        // § Deltas → D5); mirrors `fauna_feed::SourceKind::glyph` so the rail
        // and the feed badge resolve the same concept. A change here is a
        // deliberate re-brand that ripples to every app's exhaustive match.
        assert_eq!(Rail::FaunaMls.glyph(), SourceGlyph::Fox);
        assert_eq!(Rail::Smtp.glyph(), SourceGlyph::Envelope);
        // The identity-less fallback only: a bridged thread paints the glyph
        // its bridge declared (`ThreadSummary::bridge`).
        assert_eq!(Rail::Bridged.glyph(), SourceGlyph::Bridge);
    }

    #[test]
    fn only_the_rails_with_a_send_backend_name_a_wire_kind() {
        // These two strings are the wire kinds an app declares to its offline
        // gate before offering the send; they must match what the send
        // actually issues, so they are pinned literally rather than derived.
        assert_eq!(
            Rail::FaunaMls.send_wire_kind(),
            Some("fauna.conversations.channel.send")
        );
        assert_eq!(Rail::Smtp.send_wire_kind(), Some("fauna.email.send"));
        assert_eq!(
            Rail::Bridged.send_wire_kind(),
            Some("fauna.bridges.conversation.send")
        );
        // A kind that named nothing, or two rails sharing one, would let a
        // single gate decision stand for two different sends.
        let kinds: Vec<_> = [Rail::FaunaMls, Rail::Smtp, Rail::Bridged]
            .iter()
            .map(Rail::send_wire_kind)
            .collect();
        for (i, a) in kinds.iter().enumerate() {
            assert!(kinds[i + 1..].iter().all(|b| b != a), "{a:?} is shared");
        }
    }

    #[test]
    fn rail_as_str_round_trips_through_parse() {
        // The canonical name form every test-agent command (and formerly,
        // every app's own hand-duplicated `parse_rail`) uses on the wire.
        for rail in [Rail::FaunaMls, Rail::Smtp, Rail::Bridged] {
            assert_eq!(Rail::parse(rail.as_str()), rail);
        }
    }

    #[test]
    fn rail_parse_unrecognized_falls_back_to_fauna_mls() {
        // Matches the silent-fallback behavior every duplicate had.
        assert_eq!(Rail::parse("not-a-rail"), Rail::FaunaMls);
        assert_eq!(Rail::parse(""), Rail::FaunaMls);
    }

    /// The per-rail legs are gone (`ui/conversations.md` § Where logic lives →
    /// *The `Bridged` adapter*, ruling 3's deletion timing): their spellings
    /// are no rail, and an address written under one
    /// is an address of a kind this build does not name — carried with no rail
    /// (the open arm, `transport.md` § Rule 3), never re-read as another rail's.
    #[test]
    fn the_retired_per_rail_spellings_are_no_rail() {
        for retired in ["Bluesky", "ActivityPub", "Mastodon", "Nostr"] {
            assert!(
                serde_json::from_str::<Rail>(&format!("\"{retired}\"")).is_err(),
                "{retired} is no rail"
            );
        }
        for json in [
            r#"{"Bluesky":{"did":"did:plc:a","handle":"a.bsky.social"}}"#,
            r#"{"ActivityPub":{"acct":"a@b.example"}}"#,
            r#"{"Mastodon":{"acct":"a@b.example"}}"#,
            r#"{"Nostr":{"npub":"npub1xyz"}}"#,
        ] {
            let addr: TypedAddress = serde_json::from_str(json).unwrap();
            assert!(matches!(addr, TypedAddress::Unknown { .. }), "{json}");
            assert_eq!(addr.rail(), None);
        }
    }

    /// The bridged address keeps the bridge beside the far spelling, displays as
    /// the far spelling alone, and compares on both — the same far address on
    /// two bridges is two people.
    #[test]
    fn a_bridged_address_carries_its_bridge_and_displays_its_far_spelling() {
        let matrix = TypedAddress::unresolved_bridged("matrix", "@alice:example.org");
        assert_eq!(matrix.rail(), Some(Rail::Bridged));
        assert_eq!(matrix.display(), "@alice:example.org");
        assert_eq!(
            serde_json::to_string(&matrix).unwrap(),
            r#"{"Bridged":{"bridge_id":"matrix","address":"@alice:example.org"}}"#
        );
        let elsewhere = TypedAddress::unresolved_bridged("other", "@alice:example.org");
        assert!(!matrix.same_address(&elsewhere));
        assert!(matrix.same_address(&matrix.clone()));
    }

    #[test]
    fn fauna_address_displays_as_handle_at_nest() {
        let addr = TypedAddress::Fauna {
            handle: "alice@her-nest.com".into(),
            actor_id: ActorId([0u8; 32]),
        };
        assert_eq!(addr.display(), "alice@her-nest.com");
    }

    #[test]
    fn email_address_displays_as_canonical_string() {
        let addr = TypedAddress::Email {
            email_address: "alice@example.com".into(),
        };
        assert_eq!(addr.display(), "alice@example.com");
    }

    #[test]
    fn serde_round_trip_preserves_display_for_every_variant() {
        // Guards the wasm `typedAddressDisplay` export: the conversations
        // snapshot serializes `TypedAddress` (externally-tagged) and the web
        // app deserializes it back before calling `display()`. The web e2e
        // only exercises the Email rail, so this is the only coverage of the
        // `Fauna` variant's `actor_id: [u8; 32]` round-trip (a non-zero key).
        let cases = [
            TypedAddress::Fauna {
                handle: "alice@nest.example".into(),
                actor_id: ActorId([7u8; 32]),
            },
            TypedAddress::Email {
                email_address: "bob@example.com".into(),
            },
            TypedAddress::Bridged {
                bridge_id: "matrix".into(),
                address: "@carol:example.org".into(),
            },
        ];
        for addr in cases {
            let json = serde_json::to_string(&addr).expect("serialize");
            let back: TypedAddress = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, addr);
            assert_eq!(back.display(), addr.display());
        }
    }

    #[test]
    fn rail_of_address_matches_variant() {
        let fauna = TypedAddress::Fauna {
            handle: "x".into(),
            actor_id: ActorId([0u8; 32]),
        };
        assert_eq!(fauna.rail(), Some(Rail::FaunaMls));

        let bridged = TypedAddress::unresolved_bridged("nostr", "npub1...");
        assert_eq!(bridged.rail(), Some(Rail::Bridged));
    }

    #[test]
    fn same_address_email_is_case_insensitive() {
        let a = TypedAddress::Email {
            email_address: "Alice@Example.COM".into(),
        };
        let b = TypedAddress::Email {
            email_address: "alice@example.com".into(),
        };
        let c = TypedAddress::Email {
            email_address: "bob@example.com".into(),
        };
        assert!(a.same_address(&b));
        assert!(!a.same_address(&c));
    }

    #[test]
    fn same_address_other_rails_compare_exactly() {
        let a = TypedAddress::unresolved_bridged("nostr", "npub1abc");
        let b = TypedAddress::unresolved_bridged("nostr", "npub1abc");
        let c = TypedAddress::unresolved_bridged("nostr", "npub1ABC");
        assert!(a.same_address(&b));
        // No case-folding off the mail rail.
        assert!(!a.same_address(&c));
    }

    #[test]
    fn parse_email_shape() {
        let a = try_parse_typed_address("alice@example.com").unwrap();
        assert_eq!(a.rail(), Some(Rail::Smtp));
    }

    /// No app parses a bridged address (the grammar is the nest's), and the
    /// shapes the retired rails claimed are not misread as email either.
    #[test]
    fn parse_leaves_far_network_shapes_unrecognised() {
        assert!(try_parse_typed_address("did:plc:abc123").is_none());
        assert!(try_parse_typed_address("@user@instance.example").is_none());
        assert!(try_parse_typed_address("user@instance@example").is_none());
        assert!(try_parse_typed_address("npub1xyz").is_none());
    }

    #[test]
    fn parse_unrecognized_returns_none() {
        assert!(try_parse_typed_address("plain-string").is_none());
        assert!(try_parse_typed_address("").is_none());
        assert!(try_parse_typed_address("   ").is_none());
    }

    #[cfg(feature = "client-display")]
    #[test]
    fn ffi_display_matches_method_for_every_variant() {
        let cases = [
            TypedAddress::Fauna {
                handle: "alice@nest.example".into(),
                actor_id: ActorId([0u8; 32]),
            },
            TypedAddress::Email {
                email_address: "bob@example.com".into(),
            },
            TypedAddress::Bridged {
                bridge_id: "matrix".into(),
                address: "@dave:example.org".into(),
            },
        ];
        for addr in cases {
            assert_eq!(typed_address_display(&addr), addr.display());
        }
    }

    /// `unresolved_for_rail` is the inverse of `rail()` on every rail a bare
    /// string can name — the one invariant that makes it safe to build an
    /// address from a wire `(rail, raw)` pair and route on it — and answers
    /// `None` on the one it cannot, whose twin `unresolved_bridged` names the
    /// bridge too. A new `Rail` variant fails this the moment its arm is
    /// written wrong, which the two private copies this replaced could not
    /// catch (nothing tested them at all).
    #[test]
    fn unresolved_for_rail_round_trips_through_rail() {
        for rail in [Rail::FaunaMls, Rail::Smtp] {
            assert_eq!(
                TypedAddress::unresolved_for_rail(rail, "someone@example.test")
                    .and_then(|a| a.rail()),
                Some(rail),
                "{rail:?} must round-trip"
            );
        }
        assert_eq!(
            TypedAddress::unresolved_for_rail(Rail::Bridged, "someone"),
            None,
            "a bare string names no bridge"
        );
        assert_eq!(
            TypedAddress::unresolved_bridged("matrix", "someone").rail(),
            Some(Rail::Bridged)
        );
    }

    /// The placeholder the name warns about, pinned so nobody "fixes" it into
    /// something that looks resolved: the Fauna arm's actor id is zeroed.
    #[test]
    fn unresolved_for_rail_leaves_its_placeholders_visible() {
        match TypedAddress::unresolved_for_rail(Rail::FaunaMls, "alice") {
            Some(TypedAddress::Fauna { handle, actor_id }) => {
                assert_eq!(handle, "alice");
                assert_eq!(actor_id, ActorId([0u8; 32]));
            }
            other => panic!("expected a Fauna address, got {other:?}"),
        }
    }
}
