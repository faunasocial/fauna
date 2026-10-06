//! `chat.bsky` direct messages under a linked account's OAuth session — the
//! network half of the nest's Bluesky DM leg.
//!
//! Two calls, both proxied to the Bluesky chat service: [`poll_convos`] reads
//! the head page of every conversation that moved, and [`send_dm`] sends one
//! message to a peer. Neither keeps state: what was already ingested is the
//! caller's to know (the nest's deposit is idempotent on the message id), and
//! the cadence is the caller's worker's.

use atrium_api::agent::Configure;
use atrium_api::agent::bluesky::{AtprotoServiceType, BSKY_CHAT_DID};
use atrium_api::chat::bsky::convo::defs::MessageInputData;
use atrium_api::chat::bsky::convo::{
    get_convo_for_members as get_convo_for_members_ns, get_messages as get_messages_ns,
    list_convos as list_convos_ns, send_message as send_message_ns,
};
use atrium_api::types::Union;

use crate::oauth::BlueskyAgent;
use crate::translate::{translate_convo, translate_dm};
use crate::types::BlueskyDm;

/// One conversation's new head page.
#[derive(Debug, Clone)]
pub struct PolledConvo {
    pub convo_id: String,
    /// Every member's DID as the chat service lists them, the polling account
    /// included.
    pub member_dids: Vec<String>,
    /// The head page's messages, **oldest first**.
    pub messages: Vec<BlueskyDm>,
    /// The id of the conversation's newest message.
    pub head_id: String,
}

/// Route the agent's requests through the Bluesky chat service.
fn configure_chat_proxy(agent: &BlueskyAgent) {
    let did: atrium_api::types::string::Did =
        BSKY_CHAT_DID.parse().expect("BSKY_CHAT_DID is a valid DID");
    agent.configure_proxy_header(did, AtprotoServiceType::BskyChat);
}

/// List the account's conversations and fetch the head page of each one whose
/// newest message `unchanged(convo_id, head_message_id)` does not vouch for.
///
/// # Errors
/// A `listConvos` or `getMessages` failure.
pub async fn poll_convos(
    agent: &BlueskyAgent,
    unchanged: impl Fn(&str, &str) -> bool,
) -> anyhow::Result<Vec<PolledConvo>> {
    configure_chat_proxy(agent);
    let params = list_convos_ns::ParametersData {
        cursor: None,
        limit: None,
        read_state: None,
        status: None,
    };
    let output = agent
        .api
        .chat
        .bsky
        .convo
        .list_convos(params.into())
        .await
        .map_err(|e| anyhow::anyhow!("listConvos failed: {e}"))?;

    let mut polled = Vec::new();
    for convo_view in &output.convos {
        let convo = translate_convo(convo_view);
        let Some(head) = convo.last_message.as_ref() else {
            continue;
        };
        if unchanged(&convo.id, &head.id) {
            continue;
        }
        let msg_params = get_messages_ns::ParametersData {
            convo_id: convo.id.clone(),
            cursor: None,
            limit: None,
        };
        let msg_output = agent
            .api
            .chat
            .bsky
            .convo
            .get_messages(msg_params.into())
            .await
            .map_err(|e| anyhow::anyhow!("getMessages failed for convo {}: {e}", convo.id))?;
        // The API lists newest first.
        let mut messages: Vec<BlueskyDm> = msg_output
            .messages
            .iter()
            .filter_map(|msg| match msg {
                Union::Refs(get_messages_ns::OutputMessagesItem::ChatBskyConvoDefsMessageView(
                    mv,
                )) => Some(translate_dm(mv, &convo.id, &convo_view.members)),
                _ => None,
            })
            .collect();
        messages.reverse();
        polled.push(PolledConvo {
            head_id: head.id.clone(),
            member_dids: convo.members.iter().map(|m| m.did.clone()).collect(),
            convo_id: convo.id,
            messages,
        });
    }
    Ok(polled)
}

/// Send `text` to `peer_did` in the one-to-one conversation the account has
/// with them (found or opened by the chat service). Returns the sent message's
/// id.
///
/// # Errors
/// `peer_did` is no DID; a `getConvoForMembers` or `sendMessage` failure.
pub async fn send_dm(agent: &BlueskyAgent, peer_did: &str, text: &str) -> anyhow::Result<String> {
    configure_chat_proxy(agent);
    let peer: atrium_api::types::string::Did = peer_did
        .parse()
        .map_err(|e| anyhow::anyhow!("peer is not a DID: {e}"))?;
    let convo = agent
        .api
        .chat
        .bsky
        .convo
        .get_convo_for_members(
            get_convo_for_members_ns::ParametersData {
                members: vec![peer],
            }
            .into(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("getConvoForMembers failed: {e}"))?;
    let sent = agent
        .api
        .chat
        .bsky
        .convo
        .send_message(
            send_message_ns::InputData {
                convo_id: convo.convo.id.clone(),
                message: MessageInputData {
                    embed: None,
                    facets: None,
                    text: text.to_string(),
                }
                .into(),
            }
            .into(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("sendMessage failed: {e}"))?;
    Ok(sent.id.clone())
}
