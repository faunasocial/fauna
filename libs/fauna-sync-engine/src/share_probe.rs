//! A raw read of one shared set from one peer's share plane, reporting what
//! came back rather than acting on it — the e2e observable behind *"a person
//! the folder was never shared with gets nothing readable from your device"*
//! (`p2p.md` § Cross-user shared-set transfer: admission is the front door, the
//! M2 seal is the wall).
//!
//! **Why a probe and not the pump.** No app gesture makes a stranger pull a set:
//! the pump dials only sets its own seat belongs to, so a journey cannot reach
//! the serve door as a non-member through the UI. The probe is the request a
//! hostile client would send. It dials the peer named by a compare code, runs
//! the admission exchange claiming the set, and then asks for the set's rows
//! and for named manifests **whatever the admission said** — the pump stops at
//! a refusal, and a witness that stopped there too would prove only that the
//! client is polite, never that the serving side holds the door.
//!
//! **The same probe is its own control.** Run from an admitted member's seat
//! against the same peer and set, it must come back admitted, with rows, and
//! with the manifests it asked for. Only the identity differs between the two
//! runs, so a stranger's empty report cannot be a probe that never worked.
//!
//! Compiled out of release artifacts (`e2e-automation-surface-gating.md`
//! convention 15): the module exists only under `debug_assertions` or this
//! crate's `e2e-agent` feature, like [`crate::share_serve_tally`]'s recorders.

use std::sync::Arc;

use fauna_client_capabilities::group_ceremony_node::CeremonyNode;
use fauna_client_capabilities::group_ceremony_view::PeerCode;
use fauna_core::file_download::BlobFetcher;
use fauna_core::identity::ActorId;
use fauna_peer_share::admission::SetMembership;
use fauna_peer_share::client::{PeerShareBlobFetcher, admit_share_over, fetch_share_changes};
use fauna_transport::PathCandidates;

/// What one probe got back — the `offline_share_probe_set` agent command's
/// machine result.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ShareProbeReport {
    /// The dial reached the peer's listener. `false` means nothing below ran,
    /// and the report proves nothing about the serve door.
    pub dialed: bool,
    pub dial_error: Option<String>,
    /// The peer named the set among the ones it admits this seat to.
    pub admitted: bool,
    /// Why the admission exchange failed, when it did. A refused claim arrives
    /// here (the responder answers `ERR_WITNESS_REFUSED`), and the probe goes
    /// on asking regardless.
    pub admit_error: Option<String>,
    /// Change rows the peer served for the set on its first page.
    pub rows: u64,
    /// Those rows' plaintext paths — what an admitted member legitimately
    /// sees, and what a stranger must never see.
    pub paths: Vec<String>,
    pub rows_error: Option<String>,
    /// The manifest hashes the probe asked for: every one it was handed plus
    /// every one the served rows named.
    pub manifest_hashes: Vec<String>,
    /// Manifest bodies the peer answered.
    pub manifests: u64,
    /// One entry per manifest request that was refused or failed.
    pub manifest_errors: Vec<String>,
}

/// This side's half of the admission exchange admits the responder. Our own
/// verdict about the peer is not the one under test — the peer's verdict about
/// us is — and a refusing consult here would only add noise to the report.
struct AdmitAnyone;

impl SetMembership for AdmitAnyone {
    fn is_member(&self, _channel_id: &[u8; 32], _actor: &ActorId) -> bool {
        true
    }
}

/// Probe `set_id` on the peer `code` names, from `node`'s own identity, and
/// ask for each of `manifest_hashes` (hex) as well as any the served rows
/// name. Never writes anything: the rows and bodies are counted and dropped.
pub async fn probe_set(
    node: &CeremonyNode,
    code: &PeerCode,
    set_id: [u8; 32],
    manifest_hashes: &[String],
) -> ShareProbeReport {
    let mut report = ShareProbeReport::default();
    let channel = match node
        .dial_with_candidates(
            code.actor,
            PathCandidates {
                lan_endpoints: code.lan_endpoints.clone(),
                ..PathCandidates::default()
            },
        )
        .await
    {
        Ok(channel) => channel,
        Err(e) => {
            report.dial_error = Some(format!("{e:#}"));
            return report;
        }
    };
    report.dialed = true;

    match admit_share_over(Arc::clone(&channel), &[set_id], &AdmitAnyone, &code.actor.0).await {
        Ok(admission) => report.admitted = admission.admitted_by_peer.contains(&set_id),
        Err(e) => report.admit_error = Some(format!("{e:#}")),
    }

    let mut wanted: Vec<String> = manifest_hashes.to_vec();
    // `peer_is_cached_writer: true` so the provenance check refuses nothing on
    // this side: every row the peer served is counted, whatever its author.
    match fetch_share_changes(Arc::clone(&channel), &set_id, 0, &code.actor.0, true).await {
        Ok(page) => {
            let rows = page
                .accepted
                .into_iter()
                .map(|row| row.change)
                .chain(page.refused.into_iter().map(|(change, _)| change));
            for change in rows {
                report.rows += 1;
                if let Some(path) = change.path {
                    report.paths.push(path);
                }
                if let Some(hash) = change.manifest_hash
                    && !wanted.contains(&hash)
                {
                    wanted.push(hash);
                }
            }
        }
        Err(e) => report.rows_error = Some(format!("{e:#}")),
    }

    let fetcher = PeerShareBlobFetcher::new(channel, set_id);
    for hex_hash in &wanted {
        let Some(hash) = crate::share_pump::parse_hash(hex_hash) else {
            report
                .manifest_errors
                .push(format!("{hex_hash}: not a 32-byte hex hash"));
            continue;
        };
        match fetcher.fetch_manifest(&hash).await {
            Ok(_) => report.manifests += 1,
            Err(e) => report.manifest_errors.push(format!("{hex_hash}: {e:#}")),
        }
    }
    report.manifest_hashes = wanted;
    report
}

/// The `offline_share_probe_set` agent command's whole body over its wire
/// arguments: the peer's compare code as typed, the set's raw MLS group id in
/// hex, and the manifest hashes to ask for. Parsing lives here, beside
/// [`probe_set`], so every app's arm is one call and none re-derives how a
/// code or a group id is read. The code is parsed against the seat's own
/// actor, so a seat cannot probe itself by accident. `Err` is a refused
/// argument, which the arm reports on the app's error element (convention 11).
pub async fn probe_set_from_args(
    node: &CeremonyNode,
    peer_code: &str,
    group_id_hex: &str,
    manifest_hashes: &[String],
) -> Result<ShareProbeReport, String> {
    let code = fauna_client_capabilities::group_ceremony_view::parse_peer_code(
        peer_code,
        &node.own_actor(),
    )
    .map_err(|e| format!("peer_code: {e}"))?;
    let set_id = fauna_mls::types::ChannelId::from_group_id_hex(group_id_hex)
        .map_err(|e| format!("group_id_hex: {e}"))?
        .0;
    Ok(probe_set(node, &code, set_id, manifest_hashes).await)
}
