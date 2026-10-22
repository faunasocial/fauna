//! The device shell's half of the **conversations oracle's door**
//! (`docs/goal/behavior/conversations-oracle.md` § The custodian and the door).
//!
//! The shell holds no MLS leaf, no group state and no room key — the signed-in
//! first-party app on this device does, and serves the account's door. A
//! conversations tool connects to that door, proves it holds the holder key the
//! user's consent attested (the custodian seals a nonce to it; the shell opens
//! it), performs ONE operation and closes: the grant, the ceiling and the
//! record are the custodian's per operation, so a connection kept open would
//! buy nothing but a stale view of which app is serving.
//!
//! **Every refusal is relayed verbatim** — the door's typed refusal's sentence,
//! or, with nothing serving the door, `ConnectError::NoCustodian`'s one
//! sentence the model can pass on ("open Fauna on this device …"). A reply is
//! the door's own records as data, never anything the shell could open later.

use std::sync::Arc;

use fauna_ipc::oracle::{OracleOp, OracleReply};
use serde_json::{Value, json};

use crate::device::HolderKey;
use crate::tools::{ConversationOp, ToolError};

/// Who the shell is at an account's door: the account (the token's `sub`),
/// the principal row the nest minted at the consent, and the holder key it
/// attested.
pub struct DoorIdentity {
    pub actor_id: [u8; 32],
    pub principal_id: Vec<u8>,
    pub holder: Arc<HolderKey>,
}

impl DoorIdentity {
    /// The identity from the token answer's two hex ids.
    pub fn from_hex(
        actor_hex: &str,
        principal_hex: &str,
        holder: Arc<HolderKey>,
    ) -> Result<Self, ToolError> {
        let actor_id = fauna_core::hex32::decode(actor_hex)
            .map_err(|e| ToolError::new(format!("the account id {e}")))?;
        let principal_id = decode_hex(principal_hex).ok_or_else(|| {
            ToolError::new("the nest named this connection's principal in a form it cannot read")
        })?;
        Ok(Self {
            actor_id,
            principal_id,
            holder,
        })
    }
}

/// Lowercase or uppercase hex, even length.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The door's operation for a catalog one.
fn oracle_op(op: ConversationOp) -> OracleOp {
    match op {
        ConversationOp::ListRooms => OracleOp::ListRooms,
        ConversationOp::ReadRoom { room_id, before } => OracleOp::ReadRoom { room_id, before },
        ConversationOp::Search { query, room_id } => OracleOp::Search { query, room_id },
        ConversationOp::Send { room_id, body } => OracleOp::Send { room_id, body },
    }
}

fn data<T: serde::Serialize>(reply: &T) -> Result<Value, ToolError> {
    serde_json::to_value(reply).map_err(|e| ToolError::new(format!("reply did not encode: {e}")))
}

/// The door's reply as the tool's data.
fn reply_json(reply: OracleReply) -> Result<Value, ToolError> {
    match reply {
        OracleReply::Rooms(rooms) => Ok(json!({ "rooms": data(&rooms)? })),
        OracleReply::Page(page) => data(&page),
        OracleReply::Hits(hits) => Ok(json!({ "hits": data(&hits)? })),
        OracleReply::Sent(sent) => data(&sent),
    }
}

/// Perform `op` at the account's door on this device.
pub async fn perform(identity: DoorIdentity, op: ConversationOp) -> Result<Value, ToolError> {
    tokio::task::spawn_blocking(move || {
        let endpoint =
            fauna_ipc::oracle::OracleEndpoint::for_account(&identity.actor_id).map_err(|e| {
                ToolError::new(fauna_ipc::oracle::ConnectError::NoCustodian(e).to_string())
            })?;
        perform_at(&endpoint, &identity, op)
    })
    .await
    .map_err(|e| ToolError::new(format!("the door call did not finish: {e}")))?
}

/// Perform `op` at `endpoint` — [`perform`]'s blocking body, its own function
/// so a test can stand a door up at a path of its own.
#[cfg(unix)]
pub fn perform_at(
    endpoint: &fauna_ipc::oracle::OracleEndpoint,
    identity: &DoorIdentity,
    op: ConversationOp,
) -> Result<Value, ToolError> {
    let mut client = fauna_ipc::oracle::OracleClient::connect(
        endpoint,
        &identity.principal_id,
        &identity.actor_id,
        |enc, ciphertext| {
            fauna_client_conversations_oracle::open_challenge(
                identity.holder.secret(),
                &identity.actor_id,
                &identity.principal_id,
                enc,
                ciphertext,
            )
        },
    )
    .map_err(|e| ToolError::new(e.to_string()))?;
    match client
        .call(oracle_op(op))
        .map_err(|e| ToolError::new(e.to_string()))?
    {
        Ok(reply) => reply_json(reply),
        Err(refusal) => Err(ToolError::new(refusal.to_string())),
    }
}

/// The windows door's client is not built yet (`conversations-oracle.md`
/// § Implementation status today).
#[cfg(not(unix))]
pub fn perform_at(
    _endpoint: &fauna_ipc::oracle::OracleEndpoint,
    _identity: &DoorIdentity,
    _op: ConversationOp,
) -> Result<Value, ToolError> {
    // public-text: cli-answer-strings-public
    Err(ToolError::new(
        "Conversations cannot be reached on this operating system yet.",
    ))
    // public-text-end: cli-answer-strings-public
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_principal_id_is_even_length_hex() {
        assert_eq!(decode_hex("00ff7A"), Some(vec![0x00, 0xff, 0x7a]));
        for bad in ["", "abc", "zz", "0g"] {
            assert_eq!(decode_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn each_catalog_operation_is_its_doors_own() {
        let send = oracle_op(ConversationOp::Send {
            room_id: "r".into(),
            body: "hi".into(),
        });
        assert_eq!(send.class_name(), "conversation.send");
        for read in [
            ConversationOp::ListRooms,
            ConversationOp::ReadRoom {
                room_id: "r".into(),
                before: Some(3),
            },
            ConversationOp::Search {
                query: "q".into(),
                room_id: None,
            },
        ] {
            assert_eq!(oracle_op(read).class_name(), "conversation.read");
        }
    }
}
