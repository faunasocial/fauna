//! The custody-ceremony conversations glue (W8.4 (account-data-plane.md § Workstreams)) — the two adapters that
//! close the loop between the carriage seam (`fauna_conversations`) and the
//! ceremony machine (`fauna_client_capabilities::custody_ceremony`):
//!
//! * [`StoreCustodyCeremonySink`] implements the session's
//!   [`CustodyCeremonySink`] receive seam over the machine's
//!   `ingest_payload` + the account store's custody seam
//!   (`fauna.state.custody-ceremony`) — every received offer/accept/deliver
//!   is verified and durably captured in ONE read-join before the poll moves
//!   on (record-then-act's "record").
//! * [`SessionCustodyPoster`] implements the machine's
//!   [`CustodyPayloadPoster`] post door over
//!   `ConversationsSession::send_custody_payload` — how `drive_ceremonies`
//!   posts offers, accepts and witness deliveries.
//!
//! Wiring (an app or the tier_3 rig): register the sink via
//! `session.set_custody_ceremony_sink(...)`, then call `drive_ceremonies`
//! after conversation polls with this poster + the runtime handle (the
//! `CustodyRegistryWriter` impl on `AccountStoreHandle`) + the
//! `CapabilitiesClient` depositor. The `NestFolderCustodySink` /
//! `SchedulingSink` priority-#2 pattern: the conversations crate keeps no
//! custody semantics, this crate wires them once for every native app.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_client_capabilities::custody_ceremony::{
    CeremonyRecords, CustodyPayloadPoster, IngestOutcome, ReceiptOutcome, ingest_payload,
    ingest_receipt,
};
use fauna_client_config::CustodyCeremonyStore;
use fauna_conversations::ConversationsSession;
use fauna_conversations::backend::CustodyCeremonySink;
use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;

/// The ceremony-moved edge — fired by [`StoreCustodyCeremonySink`] whenever
/// an ingest actually advanced ceremony state (an offer newly pending, an
/// accept bound, a witness held), never on a duplicate. The narrowest
/// possible signal (no payload, no config handle), the
/// `FolderCustodyObserver` idiom: apps wire it to schedule a
/// `drive_ceremonies` pass; the tier_3 rig drives explicitly and registers
/// none. Called from the poll's async context — implementors notify, never
/// work inline.
pub trait CustodyCeremonyObserver: Send + Sync {
    fn ceremony_moved(&self, grant_id: &[u8]);
}

/// The store-backed [`CustodyCeremonySink`]: decode + verify + durably
/// capture one received ceremony payload into `fauna.state.custody-ceremony`
/// through the account store's custody seam.
pub struct StoreCustodyCeremonySink<S: CustodyCeremonyStore> {
    store: S,
    own_actor: ActorId,
    observer: Option<Arc<dyn CustodyCeremonyObserver>>,
}

impl<S: CustodyCeremonyStore> StoreCustodyCeremonySink<S> {
    /// `store` is the account's custody-ceremony seam — in an app the
    /// resolving seam over its account-store handle, read per payload
    /// because the store lands independently of the session; `own_actor`
    /// the reading account — the addressee bind for offers.
    pub fn new(store: S, own_actor: ActorId) -> Self {
        Self {
            store,
            own_actor,
            observer: None,
        }
    }

    /// Register the ceremony-moved observer. Optional — a sink without one
    /// only captures, and the app's next explicit drive picks the work up.
    pub fn with_observer(mut self, observer: Arc<dyn CustodyCeremonyObserver>) -> Self {
        self.observer = Some(observer);
        self
    }
}

#[async_trait]
impl<S: CustodyCeremonyStore> CustodyCeremonySink for StoreCustodyCeremonySink<S> {
    async fn custody_payload(&self, channel_hex: &str, sender: ActorId, bytes: &[u8]) -> bool {
        // Verify + capture in one read-join: `ingest_payload` mutates the
        // state only on its success paths, so a refused payload joins back
        // an unchanged state (the door puts nothing) and answers false. The
        // join keeps whatever another device captured meanwhile — the
        // upserts are idempotent per grant id by construction.
        let now = Timestamp::now();
        let own = self.own_actor;
        let result = self
            .store
            .update(|cfg| ingest_payload(cfg, &own, &sender, channel_hex, bytes, now))
            .await;
        match result {
            Ok(Ok(outcome)) => {
                if let Some(observer) = &self.observer {
                    match &outcome {
                        IngestOutcome::OfferPending { grant_id }
                        | IngestOutcome::AcceptBound { grant_id }
                        | IngestOutcome::WitnessHeld { grant_id } => {
                            observer.ceremony_moved(grant_id);
                        }
                        IngestOutcome::Duplicate => {}
                    }
                }
                true
            }
            Ok(Err(refused)) => {
                // A refused payload is a verification verdict, not a retry
                // candidate — `false` here is honest ("not captured") and the
                // debug line is the diagnosis (a duplicate grant id with
                // different bytes is the interesting one).
                tracing::debug!("custody ceremony payload refused: {refused}");
                false
            }
            Err(e) => {
                // The store door failed (not up yet, no generation tip) —
                // genuinely uncaptured; the per-launch re-walk and the
                // ceremony decay both heal.
                tracing::debug!("custody ceremony capture failed: {e}");
                false
            }
        }
    }

    /// The A7 receipt's owner-side leg (W8.7 leg 2): verify against the
    /// custodian key this account's own accept bound, and record — same one-join
    /// record-then-act shape as the ceremony above, with the registry-row write
    /// left to `drive_ceremonies` (this sink holds no writer, and the row is
    /// whole-record LWW, so one door is the correct number).
    ///
    /// `sender` is unused on purpose: the MLS-authenticated transport sender is
    /// the host *account*, while a receipt is signed by the custodian *device*
    /// the accept bound. Checking the account would be strictly weaker than the
    /// check `ingest_receipt` already makes, and would wrongly refuse the legal
    /// case of a host whose serving device differs from the device that ran the
    /// ceremony chat.
    async fn custody_receipt(&self, channel_hex: &str, _sender: ActorId, bytes: &[u8]) -> bool {
        let result = self
            .store
            .update(|cfg| ingest_receipt(cfg, channel_hex, bytes))
            .await;
        match result {
            Ok(Ok(outcome)) => {
                if let (ReceiptOutcome::Recorded { grant_id }, Some(observer)) =
                    (&outcome, &self.observer)
                {
                    // Same edge as a ceremony move: the row write is now owed,
                    // and the observer is how an app schedules the drive.
                    observer.ceremony_moved(grant_id);
                }
                true
            }
            Ok(Err(refused)) => {
                // A verification verdict, not a retry candidate. This is the
                // line that names a lying or misconfigured custodian.
                tracing::debug!("custody receipt refused: {refused}");
                false
            }
            Err(e) => {
                tracing::debug!("custody receipt capture failed: {e}");
                false
            }
        }
    }
}

/// One place the owner could send a custody offer — a 1:1 conversation with
/// the account that would host. The ceremony rides an existing conversation
/// channel (`OfferParams.channel_hex`'s contract: creating the DM is the
/// shipped user act, not ceremony business), so the candidates ARE the 1:1
/// channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyMintCandidate {
    /// The account the offer would address.
    pub host: ActorId,
    /// The conversation channel (hex) the ceremony would ride.
    pub channel_hex: String,
    /// The conversation's rendered label (the counterpart's name/handle as
    /// the thread list shows it), falling back to the channel hex when the
    /// thread has no summary yet.
    pub label: String,
}

/// Derive the owner's custody-offer host candidates from the live session:
/// every conversation channel whose MLS group has exactly two members — this
/// account and one counterpart. Deduped per counterpart (the most recently
/// listed thread wins is NOT promised; the first bound channel per host is
/// kept — offering twice to one account is a fresh grant id either way).
/// Shared here so every native app derives the same list (priority #2).
pub fn custody_mint_candidates(
    session: &ConversationsSession,
    own: &ActorId,
) -> Vec<CustodyMintCandidate> {
    let backend = session.backend();
    let engine = session.engine();
    let summaries = session.manager().snapshot().threads;
    let channels = backend
        .conv_channels()
        .into_iter()
        .map(|channel| {
            let members = engine.group_members(&channel);
            let label = backend.thread_for_channel(&channel).and_then(|tid| {
                summaries
                    .iter()
                    .find(|t| t.thread_id == tid)
                    .map(|t| t.label.clone())
            });
            (channel.to_string(), members, label)
        })
        .collect();
    mint_candidates_from_channels(channels, own)
}

/// The **pure core** of [`custody_mint_candidates`] — every rule that decides
/// the list, with the session reads already done.
///
/// Split out because the rules are the part a leg can get wrong and the session
/// is the part that cannot be built in a unit test: the two-member filter (a
/// group thread is not a 1:1 channel), the self-exclusion, the per-host dedup
/// (offering twice to one account is a fresh grant id either way, so the first
/// bound channel per host wins), the label fallback to the channel hex for a
/// thread with no summary yet, and the sort by label.
///
/// Each entry is `(channel_hex, the channel's MLS members, the thread's
/// rendered label if it has one)`.
pub fn mint_candidates_from_channels(
    channels: Vec<(String, Vec<ActorId>, Option<String>)>,
    own: &ActorId,
) -> Vec<CustodyMintCandidate> {
    let mut seen_hosts: Vec<ActorId> = Vec::new();
    let mut out = Vec::new();
    for (channel_hex, members, label) in channels {
        if members.len() != 2 {
            continue;
        }
        let Some(host) = members.into_iter().find(|m| m != own) else {
            continue;
        };
        if seen_hosts.contains(&host) {
            continue;
        }
        let label = label
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| channel_hex.clone());
        seen_hosts.push(host);
        out.push(CustodyMintCandidate {
            host,
            channel_hex,
            label,
        });
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// The machine's post door over the live conversations session.
pub struct SessionCustodyPoster(pub Arc<ConversationsSession>);

impl CustodyPayloadPoster for SessionCustodyPoster {
    async fn post(&self, channel_hex: &str, bytes: Vec<u8>) -> bool {
        match self.0.send_custody_payload(channel_hex, bytes).await {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!("custody ceremony post owed: {e}");
                false
            }
        }
    }

    async fn post_receipt(&self, channel_hex: &str, bytes: Vec<u8>) -> bool {
        match self.0.send_custody_receipt(channel_hex, bytes).await {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!("custody receipt post owed: {e}");
                false
            }
        }
    }
}

#[cfg(test)]
mod mint_candidate_tests {
    use super::*;

    fn actor(b: u8) -> ActorId {
        ActorId([b; 32])
    }

    /// A candidate is a 1:1 channel: the counterpart is the member that is not
    /// this account, and the thread's label rides along.
    #[test]
    fn a_two_member_channel_yields_its_counterpart() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![(
                "aa".to_string(),
                vec![own, actor(2)],
                Some("Bo".to_string()),
            )],
            &own,
        );
        assert_eq!(
            got,
            vec![CustodyMintCandidate {
                host: actor(2),
                channel_hex: "aa".to_string(),
                label: "Bo".to_string(),
            }]
        );
    }

    /// A group thread is not an offer channel — the ceremony rides a 1:1.
    /// Anything but exactly two members is skipped, in both directions.
    #[test]
    fn channels_that_are_not_one_to_one_are_skipped() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![
                ("group".to_string(), vec![own, actor(2), actor(3)], None),
                ("solo".to_string(), vec![own], None),
            ],
            &own,
        );
        assert!(got.is_empty(), "got {got:?}");
    }

    /// A two-member channel that does not contain this account has no
    /// counterpart to offer to — it must not yield `own` back as the host.
    #[test]
    fn a_channel_without_this_account_yields_nothing() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![("x".to_string(), vec![actor(2), actor(3)], None)],
            &own,
        );
        assert_eq!(got.len(), 1, "two other members is still a 1:1 for them");
        assert_ne!(got[0].host, own, "never offer custody to yourself");
    }

    /// Two channels with the same counterpart collapse to one candidate —
    /// the FIRST bound channel wins, whatever the labels sort to.
    #[test]
    fn one_candidate_per_host_and_the_first_channel_wins() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![
                (
                    "first".to_string(),
                    vec![own, actor(2)],
                    Some("zed".to_string()),
                ),
                (
                    "second".to_string(),
                    vec![own, actor(2)],
                    Some("abe".to_string()),
                ),
            ],
            &own,
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].channel_hex, "first");
        assert_eq!(got[0].label, "zed");
    }

    /// A thread with no summary yet — or an empty one — falls back to the
    /// channel hex rather than rendering a blank option the user cannot tell
    /// apart from another blank one.
    #[test]
    fn a_label_less_thread_falls_back_to_the_channel_hex() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![
                ("cafe".to_string(), vec![own, actor(2)], None),
                ("beef".to_string(), vec![own, actor(3)], Some(String::new())),
            ],
            &own,
        );
        let labels: Vec<&str> = got.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["beef", "cafe"]);
    }

    /// The list is sorted by label, so the picker's options do not reshuffle
    /// between opens as channel order changes.
    #[test]
    fn candidates_are_sorted_by_label() {
        let own = actor(1);
        let got = mint_candidates_from_channels(
            vec![
                (
                    "c1".to_string(),
                    vec![own, actor(2)],
                    Some("Zoe".to_string()),
                ),
                (
                    "c2".to_string(),
                    vec![own, actor(3)],
                    Some("Ada".to_string()),
                ),
                (
                    "c3".to_string(),
                    vec![own, actor(4)],
                    Some("Mel".to_string()),
                ),
            ],
            &own,
        );
        let labels: Vec<&str> = got.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["Ada", "Mel", "Zoe"]);
    }
}
