//! The standing conformance check behind the audience ladder's delegable rung:
//! **no `Secret*`-typed field may appear in any delegable-rung kind's payload
//! type** (`docs/goal/architecture/account-data-plane.md` § The audience ladder,
//! R13 (account-data-plane.md § The ratified decisions)).
//!
//! # Why a check at all, when the branch split is structural
//!
//! The key-branch split makes a fleet-only *key* unreachable from the grant
//! machinery ([`fauna_core::crypto::DelegableKindKeys`]). It cannot make a
//! *secret* unreachable, because nothing stops someone putting a credential
//! inside a kind that is registered delegable — the crypto would faithfully seal
//! it under a grantable key. The rung is a claim about a payload's *contents*,
//! and this module is what turns that claim into something the compiler holds.
//!
//! # The mechanism
//!
//! [`SecretFree`] is a marker trait implemented for the ordinary carriers of
//! non-secret data (integers, `String`, `Vec<T>`, `Option<T>`, …) and
//! **deliberately not** for [`fauna_core::secret::SecretBytes`] /
//! [`fauna_core::secret::SecretString`], the two newtypes this codebase seals
//! secrets in. A payload type earns a **pin**: a function that
//!
//! 1. destructures the type **exhaustively** — no `..` rest pattern — so adding
//!    a field to the struct fails the pin to compile until someone accounts for
//!    the new field, and
//! 2. passes every field through [`assert_secret_free`], so a field whose type is
//!    (or contains) a `Secret*` fails to compile.
//!
//! Step 1 is what makes the check *standing* rather than a snapshot: the usual
//! failure mode of a conformance test is that it keeps passing while the thing it
//! guards grows a new field it never heard of.
//!
//! [`every_delegable_kind_is_pinned`] closes the last gap — registering a kind as
//! delegable without pinning its payload type fails the test suite. And the
//! pin's *string* table and its *compiler* kind→type binding come from ONE
//! macro entry keyed by the typed constant alone, so satisfying that test without the type binding — by omitting
//! the bind, or by binding some *other* kind's constant — is unrepresentable:
//! the type-substitution hole cannot silently reopen on a sixth kind.

use fauna_core::data::{
    DelegationConfig, ModerationConfig, MutedKeyword, ParticipantRef, PersonalizationConfig,
    SyncPrefsConfig, TaskAssignment, TrainedFactorMeta,
};
use fauna_core::read_marker::ReadMarker;
use fauna_core::seen_set::{SeenRef, SeenScopeSet, SeenWatermark};

/// A type that provably carries no secret material.
///
/// Implemented for ordinary data carriers; **never** for a `Secret*` newtype.
/// Adding an impl for one would defeat the whole module, which is why the impl
/// list below is explicit rather than blanket.
pub trait SecretFree {}

/// Compile-time assertion that `T` carries no secret material.
///
/// Called once per field by each payload pin. A no-op at runtime — the work is
/// entirely in the trait bound.
pub const fn assert_secret_free<T: SecretFree + ?Sized>(_: &T) {}

macro_rules! secret_free_leaves {
    ($($t:ty),* $(,)?) => { $(impl SecretFree for $t {})* };
}

secret_free_leaves!(
    bool, char, str, String, u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize,
);

impl<T: SecretFree> SecretFree for Vec<T> {}
impl<T: SecretFree> SecretFree for Option<T> {}
impl<T: SecretFree> SecretFree for [T] {}
// A fixed-size array of an ordinary carrier is itself an ordinary carrier: the
// codebase-wide law is that secret bytes live in the `Secret*` newtypes
// (`SecretArray32` for exactly this width), and those newtypes get no impl —
// so a raw `[u8; 32]` here is a public value (a pubkey, an id), never a key.
impl<T: SecretFree, const N: usize> SecretFree for [T; N] {}
impl<T: SecretFree + ?Sized> SecretFree for &T {}
impl<T: SecretFree + ?Sized> SecretFree for Box<T> {}
impl<K: SecretFree, V: SecretFree> SecretFree for std::collections::BTreeMap<K, V> {}
impl<K: SecretFree, V: SecretFree> SecretFree for std::collections::HashMap<K, V> {}
// A carried unknown arm holds a value a newer build wrote into one of the
// pinned payload types. That build's own pin vouched for its variant before it
// could seal it under this kind's grantable key, and this build only passes
// the value through — it never builds one.
impl SecretFree for fauna_core::carried::CarriedValue {}

// Compound carriers whose SecretFree licence is the exhaustive pin below it —
// each impl is valid exactly because its pin destructures every field (or
// every variant) through `assert_secret_free`, so a `Secret*` field added to
// one of these types fails the pin to compile before this impl could vouch
// for it.
impl SecretFree for MutedKeyword {}
impl SecretFree for TrainedFactorMeta {}
impl SecretFree for TaskAssignment {}
impl SecretFree for ParticipantRef {}
impl SecretFree for SeenWatermark {}
impl SecretFree for SeenRef {}

// ── The pins ───────────────────────────────────────────────────
//
// One per delegable-rung payload type, and exactly the registered ones — a pin
// for a kind nobody grants reads as coverage it is not providing, so the two
// land together (both directions are tested below).
//
// Each destructures exhaustively (no `..`) so a new field cannot slip past
// unchecked, then asserts every field.

fn pin_moderation(v: &ModerationConfig) {
    let ModerationConfig {
        muted_keywords,
        hidden_content,
    } = v;
    assert_secret_free(muted_keywords);
    assert_secret_free(hidden_content);
}

fn pin_sync_prefs(v: &SyncPrefsConfig) {
    let SyncPrefsConfig {
        default_conflict_policy,
    } = v;
    assert_secret_free(default_conflict_policy);
}

fn pin_personalization(v: &PersonalizationConfig) {
    let PersonalizationConfig { trained_factors } = v;
    assert_secret_free(trained_factors);
}

fn pin_muted_keyword(v: &MutedKeyword) {
    let MutedKeyword { keyword, weight } = v;
    assert_secret_free(keyword);
    assert_secret_free(weight);
}

fn pin_trained_factor_meta(v: &TrainedFactorMeta) {
    let TrainedFactorMeta {
        id,
        name,
        learn_from_engagement,
        created_at,
    } = v;
    assert_secret_free(id);
    assert_secret_free(name);
    assert_secret_free(learn_from_engagement);
    assert_secret_free(created_at);
}

fn pin_delegation(v: &DelegationConfig) {
    let DelegationConfig { assignments } = v;
    assert_secret_free(assignments);
}

fn pin_task_assignment(v: &TaskAssignment) {
    let TaskAssignment {
        task_kind,
        pinned_to,
    } = v;
    assert_secret_free(task_kind);
    assert_secret_free(pinned_to);
}

// An enum's exhaustive form is the wildcard-free match: a new variant fails
// this pin to compile until someone accounts for its fields.
fn pin_participant_ref(v: &ParticipantRef) {
    match v {
        ParticipantRef::Device { device_id } => assert_secret_free(device_id),
        ParticipantRef::Nest { actor_pubkey } => assert_secret_free(actor_pubkey),
        ParticipantRef::Unknown(carried) => assert_secret_free(carried),
    }
}

// The seen-set carries scope-feed coordinates — writer ids and sequence
// numbers, i.e. references — never the referenced content and never key
// material. These pins are what let the ruling say that checkably
// (`merge_policy::KIND_SEEN_SET` documents the ruling itself).
fn pin_seen_scope_set(v: &SeenScopeSet) {
    let SeenScopeSet { watermarks, refs } = v;
    assert_secret_free(watermarks);
    assert_secret_free(refs);
}

fn pin_seen_watermark(v: &SeenWatermark) {
    let SeenWatermark { writer, seq } = v;
    assert_secret_free(writer);
    assert_secret_free(seq);
}

fn pin_seen_ref(v: &SeenRef) {
    let SeenRef { writer, seq } = v;
    assert_secret_free(writer);
    assert_secret_free(seq);
}

// The read marker: a channel reference (the entry key, blinded before it
// reaches a nest) and a counter. `merge_policy::KIND_READ_MARKER` documents
// the rung ruling this pin makes checkable.
fn pin_read_marker(v: &ReadMarker) {
    let ReadMarker { through } = v;
    assert_secret_free(through);
}

/// The kind→type binding, held by the COMPILER:
/// each delegable kind's typed constant ([`crate::merge_policy::records`]) is
/// bound here to the pin for exactly its canonical payload type. The pin
/// table below keys by *string* and so cannot see a type substitution; this
/// block can — swapping a kind's payload type means editing the `records::*`
/// constant (the generic plane doors take their `T` from it), which fails
/// this block to compile until the pin is updated to the new type, whose
/// exhaustive destructure then runs every field through [`assert_secret_free`].
/// The chain the failure scenario walked around is closed at its first
/// link.
const fn bind<T>(_kind: &crate::merge_policy::RecordKind<T>, _pin: fn(&T)) {}

/// One entry per delegable kind — and one entry emits BOTH halves of its
/// coverage: the compiler-checked [`bind`] of the
/// kind's typed constant to its pin, and the string-keyed probe row in
/// [`PINNED_DELEGABLE_PAYLOADS`].
///
/// Before this macro the two were separate hand-lists:
/// `every_delegable_kind_is_pinned` mechanically forced a table entry for
/// each registered kind, but nothing forced the matching `bind` line — a
/// sixth delegable kind could be registered with a `records::*` constant, a
/// POLICIES entry and a *string* pin while silently omitting its binding,
/// and the exact type-substitution scenario was live again for that
/// kind. The table is now generated ONLY here, so the completeness test
/// transitively enforces the binding: a kind cannot enter the table without
/// `bind` seeing its typed constant and its pin agree on the payload type.
///
/// Entry shape: `records::CONSTANT => (pin_fn, probe_closure)` — the typed
/// constant is the entry's ONLY name, and the table's string key is derived
/// from it (`.name`, the same derivation the `KIND_*` constants use). The
/// first shape of this macro took the kind string and the constant as two
/// separate arguments, and the verify-back
/// showed that was still one degree of freedom too many: a copy-pasted sixth
/// entry `KIND_FOO => (records::MODERATION, pin_moderation, …)` compiled and
/// kept every test green while `records::FOO`'s payload type was bound to
/// nothing. With the string derived from the constant, putting a kind in the
/// table *means* naming its constant, and `bind` then checks that constant's
/// payload type against the pin — a mismatch is E0308, an omission is the
/// completeness test's red, and a misbinding is unrepresentable.
///
/// The probe runs the pin (and any nested carrier-type pins) over
/// exhaustively-constructed values; the pin fn itself is what `bind`
/// type-checks against the constant.
macro_rules! delegable_payload_pins {
    ($( $record:path => ($pin:path, $probe:expr) ),* $(,)?) => {
        const _: () = {
            $( bind(&$record, $pin); )*
        };

        /// Every delegable payload type with a pin, keyed by the kind that
        /// carries it — GENERATED by [`delegable_payload_pins!`] alongside
        /// the compiler `bind` block, one entry emitting both (the macro doc
        /// owns why the two must come from one list). A registration without
        /// a pin and a pin without a registration both fail the tests below.
        /// Nested carrier types (`TrainedFactorMeta`, `TaskAssignment`,
        /// `ParticipantRef`) are pinned through their container's probe, on
        /// values constructed exhaustively so a field-add breaks the probe
        /// as well as the pin.
        pub const PINNED_DELEGABLE_PAYLOADS: &[(&str, fn())] = &[
            $( ($record.name, $probe) ),*
        ];
    };
}

delegable_payload_pins! {
    crate::merge_policy::records::MODERATION => (
        pin_moderation,
        || {
            pin_moderation(&ModerationConfig::default());
            pin_muted_keyword(&MutedKeyword::new(""));
        }
    ),
    crate::merge_policy::records::SYNC_PREFS => (
        pin_sync_prefs,
        || pin_sync_prefs(&SyncPrefsConfig::default())
    ),
    crate::merge_policy::records::PERSONALIZATION => (
        pin_personalization,
        || {
            pin_personalization(&PersonalizationConfig::default());
            pin_trained_factor_meta(&TrainedFactorMeta {
                id: Vec::new(),
                name: String::new(),
                learn_from_engagement: false,
                created_at: 0,
            });
        }
    ),
    crate::merge_policy::records::DELEGATION => (
        pin_delegation,
        || {
            pin_delegation(&DelegationConfig::default());
            pin_task_assignment(&TaskAssignment {
                task_kind: String::new(),
                pinned_to: None,
            });
            pin_participant_ref(&ParticipantRef::Device {
                device_id: String::new(),
            });
            pin_participant_ref(&ParticipantRef::Nest {
                actor_pubkey: [0u8; 32],
            });
        }
    ),
    crate::merge_policy::records::SEEN_SET => (
        pin_seen_scope_set,
        || {
            pin_seen_scope_set(&SeenScopeSet::default());
            pin_seen_watermark(&SeenWatermark {
                writer: [0u8; 32],
                seq: 0,
            });
            pin_seen_ref(&SeenRef {
                writer: [0u8; 32],
                seq: 0,
            });
        }
    ),
    crate::merge_policy::records::READ_MARKER => (
        pin_read_marker,
        || {
            pin_read_marker(&ReadMarker::default());
        }
    ),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge_policy::delegable_kinds;

    /// The standing check: a kind registered at the delegable rung must have a
    /// Secret-free pin for its payload type.
    ///
    /// Registering a delegable kind is otherwise a one-line edit that silently
    /// asserts "nothing in this payload is secret" with nothing checking it.
    #[test]
    fn every_delegable_kind_is_pinned() {
        for kind in delegable_kinds() {
            assert!(
                PINNED_DELEGABLE_PAYLOADS.iter().any(|(k, _)| *k == kind),
                "kind `{kind}` is registered at the delegable rung — a grant can hand its \
                 {{entry_key, item_blind}} pair to a third party — but its payload type has no \
                 Secret-free pin in `secret_free.rs`. Add one (exhaustive destructure, every \
                 field through `assert_secret_free`) before registering the kind, or register \
                 the kind fleet-only."
            );
        }
    }

    /// The pins are functions, so nothing forces them to be *called*; a pin that
    /// is never referenced still type-checks (its bounds are checked at
    /// definition), but running them keeps the table honest against a stale entry
    /// pointing at a deleted pin.
    #[test]
    fn all_pins_run() {
        for (_, pin) in PINNED_DELEGABLE_PAYLOADS {
            pin();
        }
    }

    /// The reverse direction: a pin must name a kind that is *actually*
    /// registered delegable.
    ///
    /// Without this, a kind moved to fleet-only (or renamed) leaves its pin
    /// behind, and [`every_delegable_kind_is_pinned`] keeps passing while the
    /// table quietly describes a world that no longer exists — the table would
    /// then be read as "these four are cleared for granting" when nothing checks
    /// that claim any more.
    #[test]
    fn every_pin_names_a_registered_delegable_kind() {
        let registered: Vec<_> = delegable_kinds().collect();
        for (kind, _) in PINNED_DELEGABLE_PAYLOADS {
            assert!(
                registered.contains(kind),
                "`{kind}` has a Secret-free pin but is not registered at the delegable rung. \
                 Either register it, or drop the pin — a pin for a kind nobody grants reads as \
                 coverage it is not providing."
            );
        }
    }
}
