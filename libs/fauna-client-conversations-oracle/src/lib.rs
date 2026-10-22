//! The conversations oracle's **custodian** — what a signed-in first-party app
//! hosting the account's conversations engine serves on the oracle's door
//! (`docs/goal/behavior/conversations-oracle.md`).
//!
//! A device principal (the cli's device shell) holds no MLS leaf and never
//! will. It holds a keyless `identity.op` grant naming `conversation.read`
//! and/or `conversation.send`, connects to the door
//! ([`fauna_ipc::oracle`]), proves its attested holder key, and asks this
//! custodian to perform operations — which it does through the page's own
//! [`ConversationsManager`], so what it serves is what the page shows and what
//! it sends is what the page would send.
//!
//! Per operation, in order (the owner doc's architectural rule 5): the grant is
//! **re-resolved** against the account's own state ([`CustodianAccount`]), the
//! ceiling ([`RateWindow`]) **binds per principal**, the operation is
//! **performed**, and it is **recorded** for the roster. No exemption for a
//! "trusted" caller exists here.
//!
//! The account's state reaches the custodian through [`CustodianAccount`] — the
//! seam each app's account runtime implements over its own stores (the owner's
//! grant log, the roster, the account-scoped store the ceiling's window rests
//! in, the record pump, the device's conversation index). Policy is this
//! crate's; the seam only reads and writes.
//!
//! [`runtime`] is that seam's one implementation over the account runtime's
//! real stores; [`serve`] holds the endpoint lock (one app serves an
//! account's door at a time) and runs the door.

use std::sync::Arc;

use fauna_conversations::{ConversationsManager, MessageSnapshot, ThreadDetail, ThreadId};
use fauna_core::identity_op::{
    GrantTupleRef, IdentityOpClass, RateWindow, WINDOW_SECS, grant_admits,
};
use fauna_ipc::oracle::{
    Challenge, ChallengeOutcome, DoorFuture, Hello, ORACLE_NONCE_LEN, ORACLE_PAGE_SIZE,
    ORACLE_SEARCH_LIMIT, ORACLE_SEND_BODY_MAX, OracleDoor, OracleMessage, OracleMessagePage,
    OracleOp, OracleRefusal, OracleReply, OracleRoom, OracleSearchHit, OracleSent, PendingProof,
};
use fauna_protocol::principals::RecordedPrincipalOp;

pub mod runtime;
#[cfg(unix)]
pub mod serve;

/// The record's detail for an operation that names no one room (the room
/// list, a device-wide search). The nest refuses an empty detail.
pub const ALL_ROOMS_DETAIL: &str = "*";

/// One grant tuple as the account's grant log holds it: class and kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTuple {
    pub class: String,
    pub kind: Option<String>,
}

/// A hit the device's own conversation index answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexHit {
    pub thread_id: ThreadId,
    pub message_id: String,
    pub timestamp_ms: i64,
}

/// The account's state, as the custodian reads it — implemented by each app's
/// account runtime. Every read is per call: nothing here is cached by the
/// custodian, so a revoke from any app bites at the next operation.
pub trait CustodianAccount: Send + Sync + 'static {
    /// The account this custodian serves.
    fn actor_id(&self) -> [u8; 32];

    /// The X25519 holder key `principal_id` attested at consent — the
    /// roster's row (the owner's grant log never names a principal id; what
    /// it decides is [`Self::live_tuples`] for the key). `None` when the
    /// principal is not this account's, attested no key, or the roster cannot
    /// be read.
    fn principal_holder<'a>(&'a self, principal_id: &'a [u8]) -> DoorFuture<'a, Option<[u8; 32]>>;

    /// Every scope tuple of every grant the owner's log holds **live** at
    /// `now` (unix seconds) whose holder is `holder` — revoked grants and
    /// windows that have not opened or have closed excluded.
    fn live_tuples<'a>(&'a self, holder: &'a [u8; 32], now: u64) -> DoorFuture<'a, Vec<LiveTuple>>;

    /// The ceiling's position for `principal_id`, from the app's
    /// account-scoped store (so it binds across restarts and across the apps
    /// that take turns serving). A store that cannot be read answers a full
    /// window — the ceiling fails closed.
    fn rate_window<'a>(&'a self, principal_id: &'a [u8]) -> DoorFuture<'a, RateWindow>;

    /// Store the ceiling's position for `principal_id`.
    fn store_rate_window<'a>(
        &'a self,
        principal_id: &'a [u8],
        window: RateWindow,
    ) -> DoorFuture<'a, ()>;

    /// Keep one record row for submission over `fauna.principals.ops.record`
    /// — a local write; the submission is the pump's ([`Self::upkeep`]), which
    /// retries from its own store, so the operation never waits on the nest.
    fn record<'a>(&'a self, principal_id: &'a [u8], op: RecordedPrincipalOp) -> DoorFuture<'a, ()>;

    /// Search the device's own conversation index.
    fn search_index<'a>(&'a self, query: &'a str, limit: u32) -> DoorFuture<'a, Vec<IndexHit>>;

    /// Unix seconds now.
    fn now(&self) -> u64;

    /// The account's periodic upkeep, run on every tick of
    /// [`serve::keep_door`] whether or not this app serves the door — the
    /// record pump's submission pass.
    fn upkeep(&self) -> DoorFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// A principal the door admitted: its roster id and the key it proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedPrincipal {
    pub principal_id: Vec<u8>,
    pub holder: [u8; 32],
}

/// The custodian of one account's conversations, on this device.
pub struct ConversationsCustodian<A: CustodianAccount> {
    account: A,
    manager: Arc<ConversationsManager>,
    /// The ceiling's read-admit-store, one operation at a time: the door
    /// serves connections concurrently, and two admits racing one stored
    /// window would both pass at the last slot.
    ceiling: tokio::sync::Mutex<()>,
}

impl<A: CustodianAccount> ConversationsCustodian<A> {
    pub fn new(account: A, manager: Arc<ConversationsManager>) -> Self {
        Self {
            account,
            manager,
            ceiling: tokio::sync::Mutex::new(()),
        }
    }

    /// The account behind the custodian.
    pub fn account(&self) -> &A {
        &self.account
    }

    /// Admit one operation of `class` for `principal`: the live grant names
    /// the class, and the ceiling has room. A refused operation does not
    /// consume the window.
    pub async fn authorize(
        &self,
        principal: &AdmittedPrincipal,
        class: IdentityOpClass,
    ) -> Result<(), OracleRefusal> {
        let now = self.account.now();
        // The principal row is re-read too: a revoke ends the row with the
        // grants, and a re-consent may have moved its key.
        match self.account.principal_holder(&principal.principal_id).await {
            Some(holder) if holder == principal.holder => {}
            _ => return Err(OracleRefusal::UnknownPrincipal),
        }
        let tuples = self.account.live_tuples(&principal.holder, now).await;
        let admitted = grant_admits(
            tuples.iter().map(|t| GrantTupleRef {
                class: &t.class,
                kind: t.kind.as_deref(),
            }),
            class,
        );
        if !admitted {
            return Err(OracleRefusal::NotGranted {
                class: class.name().to_string(),
            });
        }
        let _ceiling = self.ceiling.lock().await;
        let window = self.account.rate_window(&principal.principal_id).await;
        match window.admit(now) {
            Ok(next) => {
                self.account
                    .store_rate_window(&principal.principal_id, next)
                    .await;
                Ok(())
            }
            Err(held) => Err(OracleRefusal::CeilingReached {
                retry_after_secs: held
                    .window_start
                    .saturating_add(WINDOW_SECS)
                    .saturating_sub(now)
                    .max(1),
            }),
        }
    }

    fn class_of(op: &OracleOp) -> IdentityOpClass {
        match op {
            OracleOp::Send { .. } => IdentityOpClass::ConversationSend,
            OracleOp::ListRooms | OracleOp::ReadRoom { .. } | OracleOp::Search { .. } => {
                IdentityOpClass::ConversationRead
            }
        }
    }

    /// Authorize, perform and record one operation.
    pub async fn perform_op(
        &self,
        principal: &AdmittedPrincipal,
        op: OracleOp,
    ) -> Result<OracleReply, OracleRefusal> {
        let class = Self::class_of(&op);
        self.authorize(principal, class).await?;
        // Admitted: the operation is recorded whatever its outcome — the
        // ceiling counted it, and the owner sees what was asked.
        self.account
            .record(
                &principal.principal_id,
                RecordedPrincipalOp {
                    op_id: rand::random::<[u8; 16]>().to_vec(),
                    at: i64::try_from(self.account.now()).unwrap_or(i64::MAX),
                    class: class.name().to_string(),
                    detail: op.room_id().unwrap_or(ALL_ROOMS_DETAIL).to_string(),
                    extra: Default::default(),
                },
            )
            .await;
        match op {
            OracleOp::ListRooms => Ok(OracleReply::Rooms(self.rooms())),
            OracleOp::ReadRoom { room_id, before } => {
                let detail = self.detail(&room_id)?;
                Ok(OracleReply::Page(page(&detail, before)))
            }
            OracleOp::Search { query, room_id } => self.search(query, room_id).await,
            OracleOp::Send { room_id, body } => self.send(room_id, body).await,
        }
    }

    fn detail(&self, room_id: &str) -> Result<ThreadDetail, OracleRefusal> {
        self.manager
            .thread_detail(ThreadId(room_id.to_string()))
            .ok_or(OracleRefusal::NoSuchRoom)
    }

    fn rooms(&self) -> Vec<OracleRoom> {
        self.manager
            .snapshot()
            .threads
            .into_iter()
            .filter_map(|summary| {
                let detail = self.manager.thread_detail(summary.thread_id.clone())?;
                Some(OracleRoom {
                    room_id: summary.thread_id.0.clone(),
                    title: summary.label.clone(),
                    class: class_word(detail.capabilities.encryption).to_string(),
                    members: detail.participant_displays.clone(),
                    last_activity_ms: summary.last_activity_ms,
                    unread: summary.unread_count,
                    can_send: detail.capabilities.can_send,
                    can_search_room: detail.capabilities.can_search_room,
                })
            })
            .collect()
    }

    async fn search(
        &self,
        query: String,
        room_id: Option<String>,
    ) -> Result<OracleReply, OracleRefusal> {
        if query.trim().is_empty() {
            return Err(OracleRefusal::Refused {
                reason: "a search needs words to look for".to_string(),
            });
        }
        let Some(room_id) = room_id else {
            let hits = self
                .account
                .search_index(&query, ORACLE_SEARCH_LIMIT)
                .await
                .into_iter()
                .filter_map(|hit| {
                    let detail = self.manager.thread_detail(hit.thread_id.clone())?;
                    let (position, message) = detail
                        .messages
                        .iter()
                        .enumerate()
                        .find(|(_, m)| m.message_id.0 == hit.message_id)?;
                    Some(OracleSearchHit {
                        room_id: hit.thread_id.0,
                        position: Some(position as u64),
                        excerpt: excerpt(&message.body),
                        timestamp_ms: hit.timestamp_ms,
                    })
                })
                .collect();
            return Ok(OracleReply::Hits(hits));
        };
        let detail = self.detail(&room_id)?;
        if !detail.capabilities.can_search_room {
            return Err(OracleRefusal::Refused {
                reason: "only a community room is searched through its home nest; search \
                         all your conversations instead"
                    .to_string(),
            });
        }
        let answer = self
            .manager
            .room_search_answer(ThreadId(room_id.clone()), query)
            .await
            .map_err(|e| OracleRefusal::Refused {
                reason: e.user_detail(),
            })?;
        let hits = answer
            .hits
            .iter()
            .filter_map(|hit| match hit {
                fauna_conversations::backend::RoomSearchHitRef::Message { seq } => {
                    let id = format!("conv:{}:{seq}", answer.channel_hex);
                    let (position, message) = detail
                        .messages
                        .iter()
                        .enumerate()
                        .find(|(_, m)| m.message_id.0 == id)?;
                    Some(OracleSearchHit {
                        room_id: room_id.clone(),
                        position: Some(position as u64),
                        excerpt: excerpt(&message.body),
                        timestamp_ms: message.timestamp_ms,
                    })
                }
                // A room-restricted post is read through the post seam the
                // page's strip uses; the door serves the room's messages.
                fauna_conversations::backend::RoomSearchHitRef::Post { .. } => None,
            })
            .take(ORACLE_SEARCH_LIMIT as usize)
            .collect();
        Ok(OracleReply::Hits(hits))
    }

    async fn send(&self, room_id: String, body: String) -> Result<OracleReply, OracleRefusal> {
        if body.trim().is_empty() || body.len() > ORACLE_SEND_BODY_MAX {
            return Err(OracleRefusal::Refused {
                reason: format!("a message needs 1 to {ORACLE_SEND_BODY_MAX} bytes of text"),
            });
        }
        // Resolve first, so an unknown room is the typed refusal rather than
        // the manager's diagnostic.
        self.detail(&room_id)?;
        let thread = ThreadId(room_id.clone());
        let message_id = self
            .manager
            .send_text(thread.clone(), body)
            .await
            .map_err(|e| OracleRefusal::Refused {
                reason: match e {
                    fauna_conversations::backend::BackendError::NotSupported => {
                        "this conversation does not take messages right now".to_string()
                    }
                    other => other.user_detail(),
                },
            })?;
        let position = self
            .manager
            .thread_detail(thread)
            .and_then(|d| d.messages.iter().position(|m| m.message_id == message_id))
            .unwrap_or_default() as u64;
        Ok(OracleReply::Sent(OracleSent {
            room_id,
            message_id: message_id.0,
            position,
        }))
    }
}

impl<A: CustodianAccount> OracleDoor for ConversationsCustodian<A> {
    type Principal = AdmittedPrincipal;

    fn challenge<'a>(
        &'a self,
        hello: &'a Hello,
    ) -> DoorFuture<'a, ChallengeOutcome<AdmittedPrincipal>> {
        Box::pin(async move {
            let actor_id = self.account.actor_id();
            if hello.actor_id != actor_id {
                return Err(OracleRefusal::WrongAccount);
            }
            let holder = self
                .account
                .principal_holder(&hello.principal_id)
                .await
                .ok_or(OracleRefusal::UnknownPrincipal)?;
            let nonce = zeroize::Zeroizing::new(rand::random::<[u8; ORACLE_NONCE_LEN]>());
            let binding = proof_binding(&actor_id, &hello.principal_id);
            let (enc, ciphertext) = fauna_mls::wrapped_blob::envelope::hpke_seal(
                &holder,
                &binding,
                &binding,
                &nonce[..],
            )
            .map_err(|e| {
                tracing::warn!("sealing the oracle door's challenge failed: {e}");
                OracleRefusal::UnknownPrincipal
            })?;
            Ok((
                Challenge::Sealed { enc, ciphertext },
                PendingProof {
                    nonce,
                    principal: AdmittedPrincipal {
                        principal_id: hello.principal_id.clone(),
                        holder,
                    },
                },
            ))
        })
    }

    fn perform<'a>(
        &'a self,
        principal: &'a AdmittedPrincipal,
        op: OracleOp,
    ) -> DoorFuture<'a, Result<OracleReply, OracleRefusal>> {
        Box::pin(self.perform_op(principal, op))
    }
}

/// The HPKE info/AAD binding of the door's possession proof — the principal's
/// open must use the same.
#[must_use]
pub fn proof_binding(
    actor_id: &[u8; 32],
    principal_id: &[u8],
) -> fauna_mls::wrapped_blob::AadBinding {
    fauna_mls::wrapped_blob::AadBinding::for_oracle_door_proof(actor_id, principal_id)
}

/// The principal's half of the proof: open the challenge under its holder
/// secret. What a device principal hands [`fauna_ipc::oracle::OracleClient::
/// connect`].
#[must_use]
pub fn open_challenge(
    holder_secret: &[u8; 32],
    actor_id: &[u8; 32],
    principal_id: &[u8],
    enc: &[u8],
    ciphertext: &[u8],
) -> Option<zeroize::Zeroizing<Vec<u8>>> {
    let binding = proof_binding(actor_id, principal_id);
    fauna_mls::wrapped_blob::envelope::hpke_open(holder_secret, &binding, &binding, enc, ciphertext)
        .ok()
        .map(zeroize::Zeroizing::new)
}

/// The room's class as the door words it: who holds a key to the log.
fn class_word(encryption: fauna_conversations::ThreadEncryption) -> &'static str {
    match encryption {
        fauna_conversations::ThreadEncryption::E2E => "end-to-end",
        fauna_conversations::ThreadEncryption::NestReadable => "community",
        fauna_conversations::ThreadEncryption::TransportOnly => "transport-only",
    }
}

/// The page of `detail`'s messages before `before` (exclusive), the newest
/// page when `None`. Positions are indices into the device's copy, oldest 0.
fn page(detail: &ThreadDetail, before: Option<u64>) -> OracleMessagePage {
    let len = detail.messages.len() as u64;
    let end = before.map_or(len, |b| b.min(len));
    let start = end.saturating_sub(u64::from(ORACLE_PAGE_SIZE));
    let messages = detail.messages[start as usize..end as usize]
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.deleted)
        .map(|(i, m)| message(start + i as u64, m))
        .collect();
    OracleMessagePage {
        room_id: detail.thread_id.0.clone(),
        messages,
        earlier: (start > 0).then_some(start),
    }
}

fn message(position: u64, m: &MessageSnapshot) -> OracleMessage {
    OracleMessage {
        position,
        message_id: m.message_id.0.clone(),
        sender: fauna_conversations::typed_address_display(&m.sender),
        sender_display: m.sender_display.clone(),
        body: m.body.clone(),
        timestamp_ms: m.timestamp_ms,
        is_own: m.is_own,
    }
}

/// A search hit's excerpt: the body's first 200 characters.
fn excerpt(body: &str) -> String {
    body.chars().take(200).collect()
}

#[cfg(test)]
mod tests;
