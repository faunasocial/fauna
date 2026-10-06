//! tui's half of the **T1 body-rendered browse trigger**: reporting that a
//! record's body was handed to a visible view.
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § The replica boundary
//! → T1 and its producer decomposition. Everything downstream of the report —
//! browse classification, coordinate resolution, dedup, the class-2 put — is
//! `fauna_sync_engine::observation_intake`'s, reached through
//! `AccountStoreHandle::record_observation`. This module contributes exactly
//! one fact, the one shared Rust cannot know: *which records this terminal
//! actually showed a body for.*
//!
//! # Why the report comes from the shell, and only from the shell
//!
//! tui's element list **is** its automation registry — a page hands over every
//! element its snapshot implies, and `crate::ui` clips paint to the viewport
//! ([`crate::element`]: *"the element list IS the registry; the viewport clips
//! paint only"*). So "the page registered a `dm-message-text`" is precisely the
//! **list-buffer transit** T1 names as a non-observation: a thread of 200
//! messages registers 200 bodies while a terminal shows perhaps 20.
//!
//! The frame's own hit regions are the honest witness. [`crate::ui::render`]
//! builds them by walking the *visible band* of the scrolled paragraph, so a
//! `RowHit` exists for an element if and only if at least one of its painted
//! lines landed inside the viewport this frame. Reporting from anywhere else in
//! this app — the manager, the page's element builder, the thread snapshot —
//! would report the buffer, not the screen.
//!
//! **There is no overscan to exclude here, and that is worth stating rather
//! than leaving implied:** ratatui's `Paragraph` scroll paints the band and
//! nothing beyond it, so tui's realized set and its on-screen set coincide.
//! A shell with true overscan (a virtualized list that realizes rows above and
//! below the fold) must subtract it before reporting; this one has none to
//! subtract, so the hit list is already the answer.
//!
//! # What is *not* reported, by construction
//!
//! A bubble whose body is suppressed registers no `dm-message-text` at all —
//! deleted, legally withheld, muted-keyword collapsed, content-policy collapsed
//! and content-policy blocked all return before the body element is pushed
//! ([`crate::conversations::detail_elements`]). So the observation rides that
//! one element, and every suppression arm excludes itself without a second set
//! of conditions to keep in step.

use fauna_sync_engine::account_runtime::AccountStoreHandle;
use fauna_sync_engine::observation_intake::Observation;

use crate::app::App;
use crate::ui::{HitTarget, RowHit};

/// A record's identity on the account data plane, as an app model carries it —
/// the `(scope, record-digest)` pair `fauna_conversations::plane` derives.
///
/// Held as the conversations crate's own type rather than a
/// [`Observation`]: that crate is wire-type-free by design, and the lift into
/// typed protocol values is [`Observation::parse`]'s job at the seam below, not
/// a page's.
pub(crate) type PlaneRecord = fauna_conversations::message::PlaneRef;

/// Every record this frame actually showed a body for, deduplicated.
///
/// Reads the frame's hit regions — the viewport-clipped projection — rather
/// than the element list, which is the whole point (see the module docs). A
/// malformed plane ref is skipped silently: the seen-set is grow-only, so the
/// cost of dropping a report is at most one re-render, and an app has nothing
/// useful to say to a user about it.
pub(crate) fn observed_this_frame(app: &App, hits: &[RowHit]) -> Vec<Observation> {
    let elements = app.page_elements();
    let mut out: Vec<Observation> = Vec::new();
    for hit in hits {
        let HitTarget::Page(index) = hit.target() else {
            continue;
        };
        let Some(record) = elements.get(index).and_then(|e| e.observation.as_ref()) else {
            continue;
        };
        let Some(observation) = Observation::parse(&record.scope, &record.record_digest) else {
            continue;
        };
        // A body spans as many rows as it wraps to, so one element yields one
        // hit per painted line — report it once.
        if !out.contains(&observation) {
            out.push(observation);
        }
    }
    out
}

/// Hand this frame's observations to the shared intake.
///
/// Fire-and-forget: reporting is never on the paint path's critical line, and a
/// failure has no user-visible meaning — the set is grow-only, so the next
/// frame that paints the same body reports it again. Repeats are free by
/// contract (the intake dedups against the merged entry and publishes nothing),
/// which is exactly why this can report the whole visible set every frame
/// instead of keeping a mirror of what it already sent.
pub(crate) fn report(handle: &AccountStoreHandle, observations: Vec<Observation>) {
    if observations.is_empty() {
        return;
    }
    let handle = handle.clone();
    tokio::spawn(async move {
        for observation in observations {
            if let Err(e) = handle.record_observation(observation).await {
                tracing::debug!("T1 observation report dropped: {e:#}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL_HEX: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    /// The cross-crate pin `fauna_conversations::plane`'s module docs name.
    /// That crate builds its scope string from its own literals — it is
    /// deliberately wire-type-free and takes no `fauna-protocol` dependency —
    /// so this is the only place the two spellings actually meet. A drift on
    /// either side reds here instead of shipping observations that silently
    /// resolve nothing.
    ///
    /// It also pins the *record* half to the nest's own filing mint, so a
    /// client and the nest that stored the record agree on its identity by
    /// construction.
    #[test]
    fn a_painted_bubbles_plane_ref_parses_into_an_observation() {
        let r = fauna_conversations::plane::plane_ref(CHANNEL_HEX, b"envelope").expect("plane ref");
        let observation =
            Observation::parse(&r.scope, &r.record_digest).expect("the two spellings agree");
        assert_eq!(observation.scope.kind(), "conv");
        assert_eq!(observation.scope.scope_id(), &[0x33; 32]);
        let (minted, _bytes) = fauna_mls::segments::encode_record(
            &fauna_mls::segments::ConvRecordEnvelope::new(b"envelope".to_vec()),
        )
        .expect("mint");
        assert_eq!(
            observation.record,
            fauna_core::data::ContentHash::from_digest_dag_cbor(minted.digest()),
        );
    }

    // The reporter's own behaviour — which bodies a frame reports, and which it
    // must not — is proven against the production render path beside the page
    // that builds those elements: `crate::conversations`'s
    // `only_the_bubbles_this_frame_painted_are_reported` and its neighbours.
}
