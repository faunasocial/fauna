//! A raw chunk pull from another device's same-account peer leg, reporting
//! what came back rather than acting on it — the e2e observable behind *"only
//! your own devices can fetch files from your device over the direct path"*
//! (`p2p.md` § Wormability posture, rule 1: admission before parsing) and
//! *"what travels directly is encrypted on the way"* (rule 4: peers exchange
//! hash-addressed AEAD ciphertext).
//!
//! **Why a probe and not the dial pass.** No app gesture makes a device of
//! another account dial yours: the dial pass dials only the account's own
//! fleet, whose candidates travel over the fleet-only `device-endpoints` kind.
//! The probe is the request a hostile device would send. It dials the target
//! from this agent's own bound leg — its own node id, its own enrollment
//! witness — runs the admission exchange, and then asks for the chunk bodies
//! **whatever the admission said**: the dial pass stops at a refusal, and a
//! witness that stopped there too would prove only that the asking client is
//! polite, never that the serving device holds the door. It also sends one
//! malformed chunk request, whose answer says which ran first on the serving
//! side, the admission check or the parser.
//!
//! **The same probe is its own control.** Run from a device of the target's
//! own account, it must come back admitted and served, with the malformed
//! request meeting the parser. Only the identity differs between the two
//! runs, so a stranger's empty report cannot be a probe that never worked.
//! The shape is the share plane's stranger probe
//! (`fauna_sync_engine::share_probe`), on the same-account leg.
//!
//! Compiled out of release artifacts (`e2e-automation-surface-gating.md`
//! convention 15), as the IPC verbs that reach it are.

use std::net::SocketAddr;

use fauna_core::data::ContentHash;
use fauna_ipc::sync::{
    PeerLegDialTarget, PeerLegProbeBody, PeerLegProbeReport, PeerLegProbeRequest,
};
use fauna_peer_channel::{ChannelError, PeerChannel};
use fauna_protocol::peer_sync::{
    KIND_PEER_SYNC_ADMIT, KIND_PEER_SYNC_CHUNKS_PULL, PeerSyncAdmitRequest,
    WITNESS_DEVICE_AUTHORIZATION,
};
use fauna_protocol::{Value, decode_strict, encode_canonical};
use fauna_transport::{NestPath, PathCandidates};

use crate::peer_files::{FolderDbs, LiveLeg};

/// This device's own dial target: its node id and its LAN candidates, the
/// bound ports crossed with this machine's interface addresses — what the
/// dial pass publishes to the fleet.
pub fn dial_target(leg: &LiveLeg) -> PeerLegDialTarget {
    let lan_ips = fauna_peer_sync::discovery::discover_lan_candidates();
    PeerLegDialTarget {
        node_id: hex::encode(leg.node_id),
        lan_endpoints: fauna_core::device_endpoints::lan_socket_addrs(&leg.bound_addrs, &lan_ips),
    }
}

/// Run one probe from `leg` (see the module docs). `Err` is a refused
/// argument — a malformed node id, candidate or key — never a refusal by the
/// target, which lands in the report.
pub async fn probe(
    leg: &LiveLeg,
    folder_dbs: &FolderDbs,
    req: &PeerLegProbeRequest,
) -> Result<PeerLegProbeReport, String> {
    let node_id = fauna_core::hex32::decode(&req.target.node_id)
        .map_err(|e| format!("target.node_id: {e}"))?;
    let lan_endpoints = req
        .target
        .lan_endpoints
        .iter()
        .map(|a| {
            a.parse::<SocketAddr>()
                .map_err(|e| format!("target.lan_endpoints: {a}: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut keys: Vec<[u8; 32]> = req
        .store_keys
        .iter()
        .map(|k| fauna_core::hex32::decode(k).map_err(|e| format!("store_keys: {k}: {e}")))
        .collect::<Result<_, _>>()?;
    if let Some(path) = &req.relative_path {
        let held = folder_dbs
            .held_store_keys(&req.folder, path)
            .await
            .map_err(|e| {
                format!(
                    "relative_path: {}: {e:#}",
                    fauna_core::log_redact::log_path(path)
                )
            })?;
        for key in held {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }

    let mut report = PeerLegProbeReport {
        store_keys: keys.iter().map(hex::encode).collect(),
        ..Default::default()
    };
    let target = fauna_peer_sync::PeerDialTarget {
        node_id,
        candidates: PathCandidates {
            lan_endpoints,
            ..PathCandidates::default()
        },
        relay_url: None,
    };
    let channel =
        match fauna_sync_engine::peer_leg::dial_and_open_channel(&leg.transport, &target).await {
            Ok(channel) => channel,
            Err(e) => {
                report.dial_error = Some(format!("{e:#}"));
                return Ok(report);
            }
        };
    report.dialed = true;
    report.peer_identity = Some(hex::encode(channel.peer_identity().as_bytes()));
    report.path = Some(channel.path().label().to_string());

    // The admission exchange, raw: the target's answer is the verdict under
    // test, so this side neither evaluates the reply's witness nor stops on a
    // refusal (`fauna_peer_sync::admit_over_as` would do both).
    match request(
        &channel,
        KIND_PEER_SYNC_ADMIT,
        &PeerSyncAdmitRequest {
            witness_kind: WITNESS_DEVICE_AUTHORIZATION.to_string(),
            witness: leg.device_authorization.clone(),
            endpoints: None,
            extra: Default::default(),
        },
    )
    .await
    {
        Ok(_) => report.admitted = true,
        Err(e) => report.admit_error = Some(e),
    }

    // The production pull, with the nest path called unavailable so the
    // relay byte gate never holds a body back: every body that comes back is
    // whole and checked against its key.
    let wanted: Vec<ContentHash> = keys
        .iter()
        .map(|k| ContentHash::from_digest_raw(*k))
        .collect();
    match fauna_peer_sync::pull_file_chunks(
        &channel,
        &req.folder,
        &wanted,
        "peer-leg-probe",
        NestPath::Unavailable,
    )
    .await
    {
        Ok(pull) => {
            for key in &wanted {
                if let Some(body) = pull.bodies.get(key) {
                    report.bodies.push(PeerLegProbeBody {
                        store_key: hex::encode(key.digest()),
                        bytes: hex::encode(body),
                    });
                }
            }
            report.missing = pull
                .missing
                .iter()
                .map(|k| hex::encode(k.digest()))
                .collect();
        }
        Err(e) => report.pull_error = Some(error_code(&e).unwrap_or_else(|| format!("{e:#}"))),
    }

    // A request whose shape no door accepts: an admitted device meets the
    // parser's refusal, a device the target does not admit must meet the
    // admission refusal first.
    report.malformed_error = request(
        &channel,
        KIND_PEER_SYNC_CHUNKS_PULL,
        &("not", "a", "request"),
    )
    .await
    .err();
    Ok(report)
}

/// One request; `Err` carries the target's wire error code (or the failure,
/// when the request never got an answer).
async fn request(
    channel: &PeerChannel,
    kind: &str,
    payload: &impl serde::Serialize,
) -> Result<Value, String> {
    let value: Value = encode_canonical(payload)
        .map_err(|e| format!("encode: {e}"))
        .and_then(|bytes| decode_strict(&bytes).map_err(|e| format!("as value: {e}")))?;
    channel.request(kind, value).await.map_err(|e| match e {
        ChannelError::Rpc(rpc) => rpc.code,
        other => other.to_string(),
    })
}

/// The target's wire error code anywhere in `e`'s chain.
fn error_code(e: &anyhow::Error) -> Option<String> {
    e.chain()
        .find_map(|cause| match cause.downcast_ref::<ChannelError>() {
            Some(ChannelError::Rpc(rpc)) => Some(rpc.code.clone()),
            _ => None,
        })
}
