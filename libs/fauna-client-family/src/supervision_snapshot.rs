//! The **last-known supervision snapshot** — clause 2 of the unfetched-policy
//! ruling.
//!
//! # Why this record exists
//!
//! Clause 1 says enforcement state never moves on a failed `fauna.family.status`
//! read. That is necessary but not sufficient: it protects a floor already
//! *loaded* in memory, and says nothing about a client that has not read yet.
//! At cold launch every app starts with no floor at all, so a supervised ward
//! who launches offline renders unsupervised — and because § Screen time is
//! enforced by pure client-local clock, with no floor loaded "airplane mode"
//! becomes a bedtime-lock bypass any child can perform. The ruling's words:
//! persistence is "the distinction clause 1 cannot supply for a client that has
//! never read yet".
//!
//! So the snapshot is written on **every successful** status read and loaded at
//! launch **ahead of** the first read. Its whole job is to let a cold start tell
//! *unsupervised* (no snapshot, or one that says so) from *supervised, floor
//! unknown* (a snapshot carrying the floor).
//!
//! # Why the shape lives here
//!
//! The ruling puts "the record's shape + (de)serialization … in shared Rust —
//! `fauna-client-family`, over the `fauna-client-accounts` `SecretStore` seam
//! every app already holds — so seven apps cannot drift on it" (priority #2).
//! The storage slot itself is
//! [`AccountRegistry::supervision_snapshot_json`](../../fauna_client_accounts/struct.AccountRegistry.html)
//! and is deliberately **opaque JSON** to the registry, exactly like the three
//! pending-* slots beside it: the registry stores the string and never parses
//! it, so this module is the single owner of the format.
//!
//! # Why it is not the wire type
//!
//! [`SupervisionSnapshot`] is an **at-rest replica**, not a copy of
//! [`FamilyStatusReply`], and the two evolve under different rules
//! (`version-compatibility.md`): the wire is additive-forever and carries a
//! `#[serde(flatten)] extra` bag on every struct, while this record is local,
//! rewritten on every successful read, and needs to stay small and inspectable.
//! Embedding the wire type would also drag `serde_bytes::ByteBuf` actor ids
//! through JSON as 32-element number arrays. So the guardian is stored as
//! [`SnapshotGuardian`] with a **hex** actor id, and
//! [`SupervisionSnapshot::from_status`] is the one place the wire is folded
//! into the replica.
//!
//! # The fail direction
//!
//! An unreadable or malformed snapshot yields `None` — the same
//! "no information" that clause 1 gives a failed read, which leaves the client
//! exactly where it would have been without this feature. That is the honest
//! direction: this record may only ever *restore* a floor the nest already
//! stated, never invent one. Clause 3's declared residual (a device that has
//! never completed one successful read enforces nothing) is unchanged by it.

use fauna_core::obligation::ContentPolicy;
use fauna_core::screen_time::ScreenTimePolicy;
use fauna_protocol::family::{FamilyGuardianInfo, FamilyStatusReply};
use serde::{Deserialize, Serialize};

/// The guardian, as stored at rest: hex actor id + the handle the lock screen
/// and the supervised indicator name.
///
/// Hex rather than raw bytes because this record is JSON in a platform secret
/// store — see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SnapshotGuardian {
    /// Lowercase hex of the guardian's actor id.
    pub actor_id_hex: String,
    pub handle: String,
}

impl SnapshotGuardian {
    fn from_wire(info: &FamilyGuardianInfo) -> Self {
        Self {
            actor_id_hex: hex_encode(info.actor_id.as_slice()),
            handle: info.handle.clone(),
        }
    }

    /// Rebuild the wire shape the render surfaces already consume (the screen
    /// lock names the guardian; the supervised indicator renders the handle).
    ///
    /// A snapshot whose hex does not decode yields an **empty** actor id rather
    /// than failing the whole restore: the handle is what the two surfaces
    /// display, and dropping a real floor because an id is malformed would fail
    /// in the unsafe direction. The id is not an authorization input
    /// client-side — every family authorization is nest-side (§ The trust
    /// shape), so a blank id here can widen nothing.
    pub fn to_wire(&self) -> FamilyGuardianInfo {
        FamilyGuardianInfo {
            actor_id: fauna_protocol::ByteBuf::from(
                fauna_core::format::hex_decode(&self.actor_id_hex).unwrap_or_default(),
            ),
            handle: self.handle.clone(),
            ..Default::default()
        }
    }
}

/// What a client remembers between successful `fauna.family.status` reads.
///
/// The four fields are exactly the ruling's list — "supervised-by, the content
/// floor, `content_notify`, and the screen-time policy". The viewer's *own*
/// spam/phishing thresholds are deliberately **absent**: the ruling notes that
/// failing them toward absent only under-enforces the viewer's own collapse
/// (the benign direction) and they re-arrive with their own read.
///
/// `Default` is the unsupervised snapshot — no guardian, no floor, notify off,
/// no screen-time policy — which is also what a successful read of an
/// unsupervised account produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SupervisionSnapshot {
    /// The guardian, when the last successful read reported one. `None` is the
    /// positive statement "this account was unsupervised as of that read" — not
    /// "unknown"; unknown is the absence of the whole record.
    #[serde(default)]
    pub supervised_by: Option<SnapshotGuardian>,
    /// The guardian's per-category render floor.
    #[serde(default)]
    pub content_policy: Option<ContentPolicy>,
    /// The Guardian Notify knob (§ Guardian Notify). Defaults off — the
    /// unsupervised-equivalent.
    #[serde(default)]
    pub content_notify: bool,
    /// The guardian's usage window + daily budget (§ Screen time).
    #[serde(default)]
    pub screen_time: Option<ScreenTimePolicy>,
}

impl SupervisionSnapshot {
    /// Fold a **successful** `fauna.family.status` reply into the record to
    /// persist. Never call this with a failed read — clause 1.
    ///
    /// ⚠ **Every supervised field is gated on `supervised_by`, not merely on
    /// the policy document being present**, and that gate is the reason this
    /// derivation is shared rather than written per app. The floor is only
    /// legitimate while a guardianship exists, so a graduated ward whose last
    /// reply still carried a policy document must not keep having their feed
    /// censored by a guardian they no longer have. tui and linux each already
    /// apply this gate at their own render seams; folding it in here is what
    /// stops the remaining five apps from persisting an ungated copy and
    /// reviving the floor at their next launch — a bug that would be invisible
    /// until a real graduation.
    pub fn from_status(status: &FamilyStatusReply) -> Self {
        let guardian = status.supervised_by.as_ref();
        let policy = status.policy.as_ref().filter(|_| guardian.is_some());
        Self {
            supervised_by: guardian.map(SnapshotGuardian::from_wire),
            content_policy: policy.and_then(|p| p.content_policy),
            content_notify: policy.and_then(|p| p.content_notify) == Some(true),
            screen_time: policy.and_then(|p| p.screen_time),
        }
    }

    /// True when the last successful read reported a guardianship. The gate
    /// every consuming surface applies before enforcing anything.
    pub fn is_supervised(&self) -> bool {
        self.supervised_by.is_some()
    }

    /// Serialize for [`AccountRegistry::set_supervision_snapshot_json`].
    ///
    /// Infallible by construction — every field is a plain serde value with no
    /// map keys that can fail — so a caller never has to decide what to do with
    /// a write error at the moment it has a *good* reading to persist.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Parse a slot value written by [`Self::to_json`]. `None` for absent or
    /// malformed — see the module docs' fail direction.
    pub fn from_json(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    fauna_core::format::hex_full(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::obligation::ContentFloor;
    use fauna_protocol::ByteBuf;
    use fauna_protocol::family::ReachPolicy;

    fn guardian() -> FamilyGuardianInfo {
        FamilyGuardianInfo {
            actor_id: ByteBuf::from(vec![0xab, 0xcd, 0x01]),
            handle: "parent@example.org".to_string(),
            ..Default::default()
        }
    }

    fn full_policy() -> ReachPolicy {
        ReachPolicy {
            content_policy: Some(ContentPolicy {
                nsfw: ContentFloor::Block,
                ..Default::default()
            }),
            content_notify: Some(true),
            screen_time: Some(ScreenTimePolicy {
                window_start: Some(1260),
                window_end: Some(420),
                daily_minutes: Some(90),
            }),
            ..Default::default()
        }
    }

    fn supervised_status() -> FamilyStatusReply {
        FamilyStatusReply {
            supervised_by: Some(guardian()),
            policy: Some(full_policy()),
            ..Default::default()
        }
    }

    #[test]
    fn a_supervised_read_carries_all_four_fields() {
        let snap = SupervisionSnapshot::from_status(&supervised_status());
        assert!(snap.is_supervised());
        assert_eq!(
            snap.supervised_by.as_ref().unwrap().actor_id_hex,
            "abcd01",
            "the actor id is stored as hex, not a byte array"
        );
        assert_eq!(
            snap.supervised_by.as_ref().unwrap().handle,
            "parent@example.org"
        );
        assert_eq!(
            snap.content_policy.unwrap().nsfw,
            ContentFloor::Block,
            "the guardian's floor is what a cold launch must restore"
        );
        assert!(snap.content_notify);
        assert_eq!(snap.screen_time.unwrap().daily_minutes, Some(90));
    }

    /// The graduation rule, and the reason this derivation is shared: a reply
    /// that still carries a policy document but names NO guardian must persist
    /// nothing enforceable, or the next cold launch revives a floor the ward
    /// has already graduated out of.
    #[test]
    fn a_policy_without_a_guardian_persists_nothing_enforceable() {
        let status = FamilyStatusReply {
            supervised_by: None,
            policy: Some(full_policy()),
            ..Default::default()
        };
        let snap = SupervisionSnapshot::from_status(&status);
        assert!(!snap.is_supervised());
        assert_eq!(snap.content_policy, None, "no guardian, no floor");
        assert!(!snap.content_notify, "no guardian, no notify counting");
        assert_eq!(snap.screen_time, None, "no guardian, no lock");
        assert_eq!(
            snap,
            SupervisionSnapshot::default(),
            "an ungraduated-looking reply collapses to the unsupervised snapshot"
        );
    }

    /// A successful read reporting no guardianship is a positive fact, and
    /// persisting it is what lets the NEXT cold launch know the account is
    /// unsupervised rather than unknown (clause 3's clearing rule).
    #[test]
    fn an_unsupervised_read_persists_as_the_unsupervised_snapshot() {
        let snap = SupervisionSnapshot::from_status(&FamilyStatusReply::default());
        assert_eq!(snap, SupervisionSnapshot::default());
        let restored = SupervisionSnapshot::from_json(&snap.to_json()).unwrap();
        assert!(
            !restored.is_supervised(),
            "the record distinguishes `unsupervised` from absent — absent is `from_json` -> None"
        );
    }

    #[test]
    fn the_record_round_trips_through_the_slot_format() {
        let snap = SupervisionSnapshot::from_status(&supervised_status());
        let restored = SupervisionSnapshot::from_json(&snap.to_json())
            .expect("a record this module wrote must parse");
        assert_eq!(restored, snap);
        assert_eq!(
            restored
                .supervised_by
                .unwrap()
                .to_wire()
                .actor_id
                .as_slice(),
            &[0xab, 0xcd, 0x01],
            "the guardian id survives the hex round trip"
        );
    }

    /// The fail direction: a malformed slot is "no information", never a
    /// fabricated floor.
    #[test]
    fn a_malformed_slot_yields_no_information() {
        assert_eq!(SupervisionSnapshot::from_json("not json"), None);
        assert_eq!(SupervisionSnapshot::from_json(""), None);
    }

    /// Forward compatibility for the at-rest record: a snapshot written by a
    /// NEWER app that added a field must still restore its floor here, not fall
    /// back to "no information" (which would silently unlock a supervised ward
    /// after a downgrade).
    #[test]
    fn an_unknown_field_from_a_newer_app_still_restores_the_floor() {
        let raw = r#"{"supervised_by":{"actor_id_hex":"ab","handle":"g"},
            "content_policy":{"nsfw":"block","spam":"inherit",
            "phishing":"inherit","commercial":"inherit"},
            "content_notify":true,"screen_time":null,
            "a_field_from_the_future":{"nested":1}}"#;
        let snap = SupervisionSnapshot::from_json(raw).expect("unknown fields are ignored");
        assert!(snap.is_supervised());
        assert_eq!(snap.content_policy.unwrap().nsfw, ContentFloor::Block);
    }

    /// A guardian id that does not decode must not cost the ward their floor —
    /// the handle is what renders, and the id authorizes nothing client-side.
    #[test]
    fn a_malformed_guardian_id_does_not_drop_the_guardian() {
        let g = SnapshotGuardian {
            actor_id_hex: "zz".to_string(),
            handle: "parent".to_string(),
        };
        assert!(g.to_wire().actor_id.as_slice().is_empty());
        assert_eq!(g.to_wire().handle, "parent");
    }
}
