//! UniFFI façade for the unattested-member review surfaces
//!.
//!
//! `ConversationsManager::evict_person_everywhere` / `::handle_for_person`
//! are **already** UniFFI-exported directly on the manager object
//! (`libs/fauna-conversations/src/manager.rs`) — a native app calls those
//! two methods itself, with no wrapper needed here. What was missing, and
//! what this module adds, is the piece `succession-aftermath.md`'s
//! Implementation-status ledger names explicitly: *"`review_row_text` is
//! plain shared Rust today, reached by tui directly; Swift/Kotlin/C#/TS need
//! a UniFFI export and a wasm binding"* — reading the roster, formatting a
//! row's display text, and persisting a Keep/Remove verdict through
//! `fauna_client_config`'s member-review seam (tui reaches that crate
//! directly too; every other app needs it over this boundary).
//!
//! **The verdict is derived, never chosen — enforced structurally, not by
//! convention.** [`member_review_remove`] is the ONLY way this module lets a
//! caller reach `decide_member_review` for a Remove: it always evicts first
//! and persists only what [`fauna_conversations::eviction::CrossGroupEviction::earned_verdict`]
//! actually earned. There is no exported function that takes a verdict
//! directly, so an app cannot record `Removed` from its own reasoning — the
//! single most costly mistake this surface can make (a person still seated
//! in a group that refused eviction, silenced as "handled").
//!
//! Gated behind its own `member-review` feature (default-on, dropped from the
//! Go mail-bridge `--no-default-features` build — same shape as
//! `muted-keywords`/`sync-prefs`): the bridge has no settings UI, and every
//! export here either returns a bare `fauna_core`/`fauna_conversations` type
//! or calls the gated `FfiNestClient::nest_arc()` accessor.

use std::sync::Arc;

use fauna_client_config::{decide_member_review, load_member_reviews};
use fauna_conversations::eviction::CrossGroupEviction;
use fauna_conversations::{ConversationsManager, ThreadId};
use fauna_core::data::{
    MemberReview, MemberReviewRowText, MemberUnattestedReason, UnattestedVerdict, review_row_text,
};
use fauna_core::identity::ActorId;

use crate::{FfiError, general_err};

// The review rows rest on the succession ledger, read through this process's
// account-store handle (the filter plane's twin seam); no export takes an
// identity.
use crate::filter_marks::ledger_store;

/// 32 raw bytes → [`ActorId`] — the exhaustive `Vec<u8>` actor-id convention
/// every FFI mirror in this crate follows (`ActorId` itself carries no
/// UniFFI derive).
fn person_from_bytes(bytes: &[u8]) -> Result<ActorId, FfiError> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| FfiError::from("a person id must be exactly 32 bytes".to_string()))?;
    Ok(ActorId(arr))
}

/// One open review item, mirroring [`MemberReview`] at the FFI boundary:
/// `person` as raw bytes (the exhaustive actor-id convention), `reasons` as
/// wire strings via [`MemberUnattestedReason::as_wire`] — round-tripped
/// through [`MemberUnattestedReason::from`] on the way back in
/// [`member_review_row_text`], so an unrecognised reason a newer build wrote
/// still crosses the boundary verbatim rather than being dropped.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiMemberReview {
    pub person: Vec<u8>,
    pub reasons: Vec<String>,
}

impl From<MemberReview> for FfiMemberReview {
    fn from(r: MemberReview) -> Self {
        Self {
            person: r.person.0.to_vec(),
            reasons: r.reasons.iter().map(|x| x.as_wire().to_string()).collect(),
        }
    }
}

/// Read the open review roster every unattested-member surface renders from
/// — the FFI face of [`load_member_reviews`]. Apps **cache** what this
/// returns, since a member list paints far more often than the ledger
/// changes, and answer per-row questions from that cache — never with a
/// hand-rolled scan.
///
/// ⚠ The projection those answers come from,
/// [`fauna_core::data::is_under_review`], is **not reachable across this
/// boundary** (it carries no UniFFI export, and this doc used to point apps
/// straight at it). Use [`member_review_marks_for_thread`] instead: it asks
/// the same projection for a whole member list at once and hands back an
/// index-parallel answer.
#[fauna_uniffi_async::export]
pub async fn member_reviews_list() -> Result<Vec<FfiMemberReview>, FfiError> {
    let store = ledger_store()?;
    let reviews = load_member_reviews(&store).await.map_err(general_err)?;
    Ok(reviews.into_iter().map(Into::into).collect())
}

/// The review mark each of `thread_id`'s member chips carries —
/// **index-parallel with `ThreadDetail::participant_displays`**, hence with
/// the `thread-member-chip[i]` those displays render: the flagged person's
/// actor id (raw bytes, the exhaustive convention) at a chip under open
/// review, `None` at every other chip. An unknown thread id answers an empty
/// list, the same "nothing to paint" a thread with no participants gives.
///
/// **This is what makes the caching instruction above actually followable.**
/// [`member_reviews_list`] tells apps to cache the roster and answer per-row
/// questions with [`fauna_core::data::is_under_review`] — which is true for
/// tui and linux, who are Rust and call it directly, and was **impossible**
/// for the four apps on this boundary: `is_under_review` carries no UniFFI
/// export, so Swift/Kotlin/C# could only reach it by hand-rolling the byte
/// comparison in their own language. That is precisely the "inline
/// `roster.iter().any(..)` in seven renderers" the projection's own doc
/// warns about, and the failure mode of this whole surface is a flagged
/// person who renders unflagged. The join is answered here instead, over
/// `fauna_conversations::member_review_flags` — the same shared function
/// web's `memberReviewMarksForThread` calls.
///
/// **It answers with the id rather than a bare boolean** for the reason web's
/// twin does: the surface that renders a mark is the surface that presses
/// *Keep*, and [`member_review_keep`] is keyed on the person — a boolean
/// would send every caller back to the participant list to dig the id out
/// again. Keying a press on the rendered *handle* would be worse still: an
/// MLS roster's handles are empty by construction and attacker-chosen by
/// threat model (`succession-aftermath.md` § Propagation → *Removing a
/// flagged member*, rule 1).
///
/// Synchronous, and `roster` is the caller's **cached** [`member_reviews_list`]
/// result: a member list paints far more often than the account store changes, and
/// § Propagation fixes the two refresh points (behind the aftermath's raise,
/// and after every adjudication).
#[uniffi::export]
pub fn member_review_marks_for_thread(
    manager: Arc<ConversationsManager>,
    thread_id: String,
    roster: Vec<FfiMemberReview>,
) -> Result<Vec<Option<Vec<u8>>>, FfiError> {
    let Some(detail) = manager.thread_detail(ThreadId(thread_id)) else {
        return Ok(Vec::new());
    };
    // `reasons` plays no part in the join (presence is the whole question), so
    // only `person` is read back — but the parameter stays the full record
    // rather than a bare id list, so a caller hands back exactly what
    // `member_reviews_list` gave it instead of projecting a field first.
    let roster = roster
        .iter()
        .map(|r| {
            Ok(MemberReview {
                person: person_from_bytes(&r.person)?,
                reasons: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    let flags = fauna_conversations::member_review_flags(&detail.participants, &roster);
    Ok(detail
        .participants
        .iter()
        .zip(flags)
        .map(|(addr, flagged)| {
            // `flagged` is only ever true for an address that HAS an actor id,
            // so the inner `None` is unreachable rather than a silent drop —
            // and it stays expressed as one so a future rail change cannot turn
            // it into a wrong-person mark.
            flagged
                .then(|| addr.person_actor_id().map(|p| p.0.to_vec()))
                .flatten()
        })
        .collect())
}

/// The display text parts for one review row — the FFI face of
/// [`review_row_text`]. `handle` is whatever
/// `ConversationsManager::handle_for_person` resolved for `person` (call it
/// first — it reads live membership, so resolving it *after* a successful
/// [`member_review_remove`] would find no seat left to read a handle off);
/// `None` is an ordinary answer, not a failure, and the row still renders
/// under the "no longer in any of your groups" wording.
#[uniffi::export]
pub fn member_review_row_text(
    person: Vec<u8>,
    reasons: Vec<String>,
    handle: Option<String>,
) -> Result<MemberReviewRowText, FfiError> {
    let review = MemberReview {
        person: person_from_bytes(&person)?,
        reasons: reasons
            .iter()
            .map(|r| MemberUnattestedReason::from(r.as_str()))
            .collect(),
    };
    Ok(review_row_text(&review, handle.as_deref()))
}

/// Record **Keep** — the owner recognises this person, so every open item
/// for them closes with no group changes. Returns whether anything was
/// actually open: a concurrent device may have already answered, and that is
/// a success no-op, never an error.
#[fauna_uniffi_async::export]
pub async fn member_review_keep(person: Vec<u8>) -> Result<bool, FfiError> {
    let store = ledger_store()?;
    let person = person_from_bytes(&person)?;
    decide_member_review(&store, &person, UnattestedVerdict::Kept)
        .await
        .map_err(general_err)
}

/// Record **Remove** — evict `person` from every group of the owner's they
/// are currently in **now** (re-derived, never from the stored item), then
/// persist whatever verdict the eviction *earned*
/// ([`CrossGroupEviction::earned_verdict`]) — never a verdict this function
/// chooses. A partial eviction earns none, so the review item stays open;
/// the returned [`CrossGroupEviction`]'s `evicted`/`failed`/`unreachable`
/// fields are what a caller composes its own message from — the derivation
/// is shared, the wording is per-app, same as every other feature (tui's own
/// composition is the reference: `apps/fauna-tui/src/settings/mod.rs`'s
/// `Op::MemberReviewRemove` arm, `succession-aftermath.md` § Propagation
/// rule (5) for what each blocking class means).
///
/// `manager` drives the eviction itself — `ConversationsManager` is already
/// UniFFI-exported — so this function's only job beyond that call is the
/// verdict persistence `evict_person_everywhere` alone cannot do (it has no
/// account-store access): the one seam that can write `Removed` calls the one
/// function that can earn it.
#[fauna_uniffi_async::export]
pub async fn member_review_remove(
    manager: Arc<ConversationsManager>,
    person: Vec<u8>,
) -> Result<CrossGroupEviction, FfiError> {
    let person = person_from_bytes(&person)?;
    let eviction = manager.evict_person_everywhere(&person).await;
    if let Some(verdict) = eviction.earned_verdict() {
        let store = ledger_store()?;
        decide_member_review(&store, &person, verdict)
            .await
            .map_err(general_err)?;
    }
    Ok(eviction)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person_bytes(b: u8) -> Vec<u8> {
        vec![b; 32]
    }

    #[test]
    fn person_from_bytes_round_trips_and_rejects_the_wrong_length() {
        let id = person_from_bytes(&person_bytes(7)).expect("32 bytes is valid");
        assert_eq!(id.0, [7u8; 32]);
        assert!(person_from_bytes(&[1, 2, 3]).is_err());
    }

    #[test]
    fn ffi_member_review_carries_the_wire_reason_strings() {
        let review = MemberReview {
            person: ActorId([9u8; 32]),
            reasons: vec![
                MemberUnattestedReason::CompromiseWindow,
                MemberUnattestedReason::Other("from-a-newer-build".to_string()),
            ],
        };
        let ffi: FfiMemberReview = review.into();
        assert_eq!(ffi.person, vec![9u8; 32]);
        assert_eq!(
            ffi.reasons,
            vec![
                "compromise_window".to_string(),
                "from-a-newer-build".to_string()
            ]
        );
    }

    /// The FFI row-text face must agree with `fauna_core::data::review_row_text`
    /// exactly — this is a thin pass-through, and a mutation that changes the
    /// result must not survive.
    #[test]
    fn member_review_row_text_matches_the_shared_function() {
        let person = person_bytes(3);
        let reasons = vec!["compromise_window".to_string()];
        let via_ffi =
            member_review_row_text(person.clone(), reasons.clone(), None).expect("valid input");

        let review = MemberReview {
            person: person_from_bytes(&person).unwrap(),
            reasons: reasons
                .iter()
                .map(|r| MemberUnattestedReason::from(r.as_str()))
                .collect(),
        };
        let direct = review_row_text(&review, None);
        assert_eq!(via_ffi, direct);
    }

    /// An unrecognised reason string must round-trip verbatim, not collapse to
    /// the compromise-window wording — the fail-visible rule
    /// `fauna_core::data`'s own docs call out.
    #[test]
    fn an_unknown_reason_string_still_resolves_a_row() {
        let text = member_review_row_text(
            person_bytes(1),
            vec!["something-a-newer-build-invented".to_string()],
            Some("alice".to_string()),
        )
        .expect("valid input");
        assert_eq!(text.reasons.len(), 1);
    }

    #[test]
    fn member_review_row_text_rejects_a_malformed_person() {
        assert!(member_review_row_text(vec![1, 2], vec![], None).is_err());
    }
}
