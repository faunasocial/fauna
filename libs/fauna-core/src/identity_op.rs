//! The `identity.op` capability class — the oracle's vocabulary.
//!
//! Owner: `docs/goal/architecture/key-material-hierarchy.md` § Audience:
//! deployment infrastructure → *The oracle* (TP11) and § Architectural rules
//! #9; the class beside its siblings: `encryption-at-rest.md` § Capability
//! tiering → *Third-party holders*. A third-party principal never holds an
//! identity key. What it may hold is a **keyless** `identity.op` grant naming
//! an *operation class*; the key's first-party custodian performs that class
//! against the live grant row, and nothing else.
//!
//! Three things live here, and each is a closed table a mind widens on
//! purpose:
//!
//! * [`IdentityOpClass`] — the operation classes the oracle performs. One
//!   variant per class that is BUILT **and carried by the Fauna family**;
//!   the goal doc's table is the plan, this enum is the fact. A class enters
//!   here in the same change as the custodian code that performs it. A table
//!   row the protocol's own door carries never enters: `atproto.commit` is
//!   the PDS write surface under ATProto OAuth `repo:` scopes (the owner
//!   doc's *The ATProto classes ride the protocol's own door*, ruled
//!   2026-10-02), so it is no variant and no scope qualifier — pinned below.
//! * [`SovereignOp`] — **the deny list, stated as code and not as an
//!   omission.** Anything that changes *who the user is* — PLC rotation,
//!   handle and domain changes, key succession, the MLS identity root — is
//!   user-device-only and never delegable. The list is an enum so the deny
//!   site is enumerated, and a compile-time assertion ([`DENY_LIST_PINNED`])
//!   holds the two tables disjoint: adding an operation class whose name is a
//!   sovereign operation's does not compile. The pin is red-verified by
//!   adding such an arm and watching the build fail, never by reading.
//! * The two predicates every custodian applies before it signs:
//!   [`grant_admits`] (does the principal's live grant name this class?) and
//!   [`RateWindow`] (the hard-constant ceiling). Window liveness is
//!   [`window_live`]. Each custodian adds its own class-specific policy on top
//!   (the Nostr custodian's kind set lives beside the NIP-46 code, in
//!   `fauna_bridge_nostr::nip46`), and records every operation it performs.
//!
//! Pure, wasm-clean, identity-free: the issuer's scope grammar
//! (`fauna_bridge_atproto::fauna_scope`) validates a scope qualifier with
//! [`IdentityOpClass::parse`], the user's device mints the tuple with
//! [`IdentityOpClass::name`], and the nest custodian decides with the
//! predicates — one vocabulary, three readers.

/// The `class` string of an `identity.op` scope tuple
/// (`fauna_mls::wrapped_blob::ScopeTuple::CLASS_IDENTITY_OP` re-exports it).
/// The tuple's `kind` carries the operation class's [`IdentityOpClass::name`];
/// the tuple is keyless — it appears in a grant's `scope` and never in its
/// `wrapped_keys`.
pub const CLASS: &str = "identity.op";

/// The first-party custodian that holds the key an operation class is
/// performed against. A class has exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Custodian {
    /// The deposited Nostr `nsec`, sealed under the nest-internal
    /// key-encryption key; performs through the nest's own NIP-46 signer
    /// (`docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer).
    NostrDepositedKey,
}

/// An operation class the oracle performs — the built rows of the goal doc's
/// table. Closed: a new class is a new variant, added in the same change as
/// the custodian code that performs it, and never one of [`SovereignOp`]'s
/// names (the compile-time pin below).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentityOpClass {
    /// `nostr.sign_event` — sign a Nostr event with the deposited key, under
    /// the custodian's kind policy.
    NostrSignEvent,
    /// `nostr.nip44` — NIP-44 encrypt / decrypt with the deposited key.
    NostrNip44,
}

impl IdentityOpClass {
    /// Every built class, in table order.
    pub const ALL: &'static [IdentityOpClass] =
        &[IdentityOpClass::NostrSignEvent, IdentityOpClass::NostrNip44];

    /// The class's name — the `identity.op` tuple's `kind` and the scope
    /// string's qualifier (`fauna:identity:op:<name>`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            IdentityOpClass::NostrSignEvent => "nostr.sign_event",
            IdentityOpClass::NostrNip44 => "nostr.nip44",
        }
    }

    /// The custodian that performs the class. Exhaustive on purpose: a class
    /// with no custodian cannot be written down.
    #[must_use]
    pub const fn custodian(self) -> Custodian {
        match self {
            IdentityOpClass::NostrSignEvent | IdentityOpClass::NostrNip44 => {
                Custodian::NostrDepositedKey
            }
        }
    }

    /// The consent card's row for a scope naming this class: what the token
    /// permits, and that no key is shared — the key stays with its custodian.
    #[must_use]
    pub const fn card_row(self) -> &'static str {
        match self {
            IdentityOpClass::NostrSignEvent => {
                "Ask your nest to sign Nostr posts and events with your Nostr key (the key itself is never shared)"
            }
            IdentityOpClass::NostrNip44 => {
                "Ask your nest to encrypt and decrypt Nostr direct messages with your Nostr key (the key itself is never shared)"
            }
        }
    }

    /// The class `name` denotes — or why it denotes none. The sovereign deny
    /// list is checked FIRST, so a sovereign name is refused as such even if
    /// a future variant were to collide with it (which the compile-time pin
    /// already forbids).
    pub fn parse(name: &str) -> Result<IdentityOpClass, IdentityOpRefusal> {
        if let Some(op) = SovereignOp::ALL
            .iter()
            .copied()
            .find(|op| op.name() == name)
        {
            return Err(IdentityOpRefusal::Sovereign(op));
        }
        IdentityOpClass::ALL
            .iter()
            .copied()
            .find(|class| class.name() == name)
            .ok_or(IdentityOpRefusal::Unknown)
    }
}

/// **The sovereign deny list.** Operations that change *who the user is*.
/// None of these is ever an [`IdentityOpClass`]: not grantable, not
/// requestable, not performable by any custodian for any principal. The enum
/// exists so the refusal is enumerated code rather than an omission — and so
/// [`DENY_LIST_PINNED`] can hold it disjoint from the class table at compile
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SovereignOp {
    /// An ATProto PLC operation (rotation keys, verification methods, the
    /// DID document) — `atproto.plc_rotate`.
    PlcRotation,
    /// Changing the account's handle — `identity.handle`.
    HandleChange,
    /// Changing the account's domain — `identity.domain`.
    DomainChange,
    /// Key succession (`docs/goal/behavior/identity-succession.md`) —
    /// `identity.succession`.
    KeySuccession,
    /// The MLS identity root — `mls.identity_root`.
    MlsIdentityRoot,
}

impl SovereignOp {
    /// Every sovereign operation.
    pub const ALL: &'static [SovereignOp] = &[
        SovereignOp::PlcRotation,
        SovereignOp::HandleChange,
        SovereignOp::DomainChange,
        SovereignOp::KeySuccession,
        SovereignOp::MlsIdentityRoot,
    ];

    /// The name a scope string or grant tuple would have to use to ask for
    /// the operation — and is refused for.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SovereignOp::PlcRotation => "atproto.plc_rotate",
            SovereignOp::HandleChange => "identity.handle",
            SovereignOp::DomainChange => "identity.domain",
            SovereignOp::KeySuccession => "identity.succession",
            SovereignOp::MlsIdentityRoot => "mls.identity_root",
        }
    }
}

/// Why a name denotes no operation class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityOpRefusal {
    /// The name is on the sovereign deny list — never delegable.
    Sovereign(SovereignOp),
    /// The name is not a built class.
    Unknown,
}

impl std::fmt::Display for IdentityOpRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityOpRefusal::Sovereign(op) => write!(
                f,
                "{} is a sovereign operation and is never delegable",
                op.name()
            ),
            IdentityOpRefusal::Unknown => f.write_str("not an identity.op class"),
        }
    }
}

const fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn deny_list_disjoint_from_classes() -> bool {
    let mut i = 0;
    while i < IdentityOpClass::ALL.len() {
        let mut j = 0;
        while j < SovereignOp::ALL.len() {
            if bytes_eq(
                IdentityOpClass::ALL[i].name().as_bytes(),
                SovereignOp::ALL[j].name().as_bytes(),
            ) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// **The enumerated deny site.** Evaluated at compile time: an
/// [`IdentityOpClass`] variant whose name is a [`SovereignOp`]'s fails the
/// build here, so the deny list cannot be hollowed out by adding the
/// operation under its own name. Red-verified 2026-10-02 by adding a
/// `PlcRotate` class named `atproto.plc_rotate`.
pub const DENY_LIST_PINNED: () = assert!(
    deny_list_disjoint_from_classes(),
    "an identity.op class is named like a sovereign operation — the deny list forbids it"
);

/// A grant scope tuple as the admission predicate reads it: its class and
/// kind strings. Built from `fauna_mls::wrapped_blob::ScopeTuple` by the
/// custodian without this crate depending on that one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantTupleRef<'a> {
    pub class: &'a str,
    pub kind: Option<&'a str>,
}

/// Does a grant whose declared scope is `tuples` admit `class`? Exactly one
/// shape admits: a tuple of class [`CLASS`] whose kind is the class's
/// [`IdentityOpClass::name`]. No prefix, no wildcard, no "any".
pub fn grant_admits<'a>(
    tuples: impl IntoIterator<Item = GrantTupleRef<'a>>,
    class: IdentityOpClass,
) -> bool {
    tuples
        .into_iter()
        .any(|t| t.class == CLASS && t.kind == Some(class.name()))
}

/// Is a grant window `[start, end]` (unix seconds) live at `now`? Both ends
/// bind: a post-dated grant admits nothing until its start.
#[must_use]
pub const fn window_live(start: u64, end: u64, now: u64) -> bool {
    start <= now && now < end
}

/// The oracle's rate ceiling — operations per principal per window. A hard
/// constant (no configuration surface): a principal that needs more is a
/// bridge, and a bridge batches.
pub const OPS_PER_WINDOW: u32 = 60;
/// The ceiling's window, in seconds.
pub const WINDOW_SECS: u64 = 60;

/// A principal's position in the current fixed window — two integers a
/// custodian keeps beside its client row, so the ceiling binds across
/// restarts and across every transport the custodian answers on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateWindow {
    /// Unix seconds at which the current window opened.
    pub window_start: u64,
    /// Operations admitted in the current window.
    pub count: u32,
}

impl RateWindow {
    /// Admit one operation at `now`: `Ok(next)` with the position to store,
    /// or `Err(self)` when the ceiling refuses (store nothing — a refused
    /// operation does not consume the window).
    pub fn admit(self, now: u64) -> Result<RateWindow, RateWindow> {
        let rolled = now >= self.window_start.saturating_add(WINDOW_SECS);
        let (window_start, count) = if rolled {
            (now, 0)
        } else {
            (self.window_start, self.count)
        };
        if count >= OPS_PER_WINDOW {
            return Err(self);
        }
        Ok(RateWindow {
            window_start,
            count: count + 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_parses_from_its_own_name_and_names_a_custodian() {
        for class in IdentityOpClass::ALL {
            assert_eq!(IdentityOpClass::parse(class.name()), Ok(*class));
            // Exhaustive by construction; the assertion pins the Nostr pair.
            assert_eq!(class.custodian(), Custodian::NostrDepositedKey);
            assert!(class.card_row().contains("never shared"));
        }
    }

    /// The deny list, read back as refusals: every sovereign name is refused
    /// AS sovereign — not as merely unknown — and is no class.
    #[test]
    fn every_sovereign_operation_is_refused_by_name() {
        for op in SovereignOp::ALL {
            assert_eq!(
                IdentityOpClass::parse(op.name()),
                Err(IdentityOpRefusal::Sovereign(*op)),
                "{}",
                op.name()
            );
            assert!(
                IdentityOpClass::ALL.iter().all(|c| c.name() != op.name()),
                "{} must not be a class",
                op.name()
            );
        }
        // The compile-time pin is live (reading the const forces evaluation).
        #[allow(clippy::let_unit_value)]
        let _pinned = DENY_LIST_PINNED;
    }

    #[test]
    fn unknown_and_near_miss_names_are_refused_as_unknown() {
        for bad in [
            "",
            "nostr",
            "nostr.sign",
            "nostr.sign_event.1",
            "NOSTR.SIGN_EVENT",
            "nostr.nip04",
            "identity.op",
        ] {
            assert_eq!(
                IdentityOpClass::parse(bad),
                Err(IdentityOpRefusal::Unknown),
                "{bad:?}"
            );
        }
    }

    /// `atproto.commit` is a row of the oracle's table and deliberately NOT a
    /// class here: the ATProto family carries it at the PDS write surface
    /// (`key-material-hierarchy.md` § The oracle → *The ATProto classes ride
    /// the protocol's own door*). A Fauna-family scope naming it must stay
    /// `invalid_scope` at PAR, so the name parses as unknown — not as a class,
    /// and not as sovereign either (a repo write is delegable; only the PLC
    /// operation is not).
    #[test]
    fn the_atproto_commit_class_is_never_a_fauna_family_qualifier() {
        assert_eq!(
            IdentityOpClass::parse("atproto.commit"),
            Err(IdentityOpRefusal::Unknown)
        );
        assert!(
            IdentityOpClass::ALL
                .iter()
                .all(|c| !c.name().starts_with("atproto.commit"))
        );
    }

    /// `mail.dkim` is a row of the oracle's table and deliberately NOT a class
    /// here either: a DKIM key is the deployment's, no user can mint a grant
    /// over a key that is not theirs, and the class is carried by the outbound
    /// spool's hand-out (`key-material-hierarchy.md` § The oracle → *The DKIM
    /// class is the outbound spool's own door*). So the name parses as unknown
    /// — not as a class, and not as sovereign (a third party sends mail through
    /// a mail door, and its message is signed where anyone's is).
    #[test]
    fn the_mail_dkim_class_is_never_a_fauna_family_qualifier() {
        assert_eq!(
            IdentityOpClass::parse("mail.dkim"),
            Err(IdentityOpRefusal::Unknown)
        );
        assert!(
            IdentityOpClass::ALL
                .iter()
                .all(|c| !c.name().starts_with("mail."))
        );
    }

    #[test]
    fn a_grant_admits_exactly_the_class_its_tuple_names() {
        let sign = GrantTupleRef {
            class: CLASS,
            kind: Some("nostr.sign_event"),
        };
        let label_write = GrantTupleRef {
            class: "content.label-write",
            kind: None,
        };
        let wrong_class_right_kind = GrantTupleRef {
            class: "content.read",
            kind: Some("nostr.sign_event"),
        };
        let kindless = GrantTupleRef {
            class: CLASS,
            kind: None,
        };
        assert!(grant_admits(
            [label_write, sign],
            IdentityOpClass::NostrSignEvent
        ));
        assert!(!grant_admits([sign], IdentityOpClass::NostrNip44));
        assert!(!grant_admits(
            [wrong_class_right_kind, kindless, label_write],
            IdentityOpClass::NostrSignEvent
        ));
        assert!(!grant_admits([], IdentityOpClass::NostrSignEvent));
    }

    #[test]
    fn the_window_binds_at_both_ends() {
        assert!(window_live(10, 20, 10));
        assert!(window_live(10, 20, 19));
        assert!(!window_live(10, 20, 20));
        assert!(!window_live(10, 20, 9));
    }

    #[test]
    fn the_ceiling_admits_a_window_and_rolls() {
        let mut w = RateWindow::default();
        let t0 = 1_000_000;
        for i in 0..OPS_PER_WINDOW {
            w = w
                .admit(t0 + u64::from(i % 7))
                .unwrap_or_else(|_| panic!("op {i} admitted"));
        }
        assert_eq!(w.count, OPS_PER_WINDOW);
        // The ceiling refuses without consuming.
        let refused = w.admit(t0 + 30).unwrap_err();
        assert_eq!(refused, w);
        // The next window opens at WINDOW_SECS and starts the count over.
        let next = w.admit(t0 + WINDOW_SECS).expect("new window admits");
        assert_eq!(
            next,
            RateWindow {
                window_start: t0 + WINDOW_SECS,
                count: 1
            }
        );
    }
}
