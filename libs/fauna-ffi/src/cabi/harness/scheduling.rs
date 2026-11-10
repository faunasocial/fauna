//! The e2e harness's **client-kept scheduling** exports.
//!
//! An inbound iTIP message is applied by the RECIPIENT's client, never the
//! nest: the shared `fauna_client_caldav` apply decides who may create, change
//! or answer an event (`inbound-scheduling-authority.md` § Who may mutate an
//! existing event over the inbound rail), and the nest only supplies the facts
//! it checks — the record author it attests on `channel.fetch`, and the mail
//! door's `X-Fauna-Authenticated-Sender` stamp. So the `[nest]` outcomes that
//! rule keeps are witnessed through these exports (`feature-catalog.md` § The
//! two surfaces): an organizer's event stored through the shared seal-and-put,
//! read back through the shared decode, and one iMIP at a time built by the
//! shared builders, carried over the real rail and applied by the shared apply
//! on the recipient's side. The Rust pin of the same flow is
//! `bins/fauna-nest/tests/conformance_caldav_scheduling_mailbox_less.rs`.

use std::collections::HashMap;
use std::ffi::c_char;
use std::sync::{Arc, Mutex};

use fauna_client::{AuthClient, NestClient, NestClientError};
use fauna_core::identity::ActorKeypair;

use fauna_conversations::backend::ConversationsRpc;

use super::{CloneKeypair, Outcome, decode_call, encode_err, err, run, with_client};
use crate::FfiError;
use crate::cabi::FfiBuffer;

/// The pace of [`deliver_over_the_sealed_rail`]'s wait for the delivered
/// Welcome and its iMIP — a poll of the nest, never an assertion's clock.
const SCHEDULING_POLL_PACE: std::time::Duration = std::time::Duration::from_millis(200);

/// How long [`deliver_over_the_sealed_rail`] waits for the iMIP it just
/// delivered to come back off the recipient's drain. Same-nest, the Welcome
/// and the record are written before the delivery returns, so the first pass
/// normally finds both; the budget only absorbs a loaded box.
const SCHEDULING_RECEIVE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// One stored event as [`fauna_harness_caldav_events`] answers it: `[uid,
/// etag, summary, [[email, partstat], …], ics]`.
type EventRow = (String, String, String, Vec<(String, String)>, String);

fn msek_from(bytes: &[u8], what: &str) -> Result<[u8; 32], FfiError> {
    bytes.try_into().map_err(|_| {
        err(format!(
            "{what} must be exactly 32 bytes, got {}",
            bytes.len()
        ))
    })
}

/// The roster a test names as bare addresses, as the organizer's client
/// holds it before anyone answered.
fn invited_roster(addresses: &[String]) -> Vec<fauna_client_caldav::AttendeeInfo> {
    addresses
        .iter()
        .map(|email| fauna_client_caldav::AttendeeInfo {
            name: String::new(),
            email: email.clone(),
            partstat: "NEEDS-ACTION".into(),
            fauna_status: "invited".into(),
        })
        .collect()
}

fn event_fields(
    uid: String,
    summary: String,
    dtstart: String,
    dtend: String,
) -> fauna_client_caldav::EventFields {
    fauna_client_caldav::EventFields {
        summary,
        dtstart,
        dtend,
        uid,
        ..Default::default()
    }
}

/// Every event of the actor's `Personal` calendar, decoded with its key —
/// empty when the calendar does not exist yet (an attendee before their first
/// invite materialized).
async fn personal_events(
    caldav: &fauna_client_caldav::CalDavClient<Arc<NestClient>>,
    actor: &[u8; 32],
    msek: &[u8; 32],
) -> Result<Vec<fauna_client_caldav::DecodedEvent>, FfiError> {
    use fauna_client_caldav::{DavKind, DavRecipientKeys, DecodedEventsPage};
    let query = fauna_protocol::bridge_routing::QueryEventsRequest {
        actor_id: actor.to_vec(),
        calendar_id: fauna_client_caldav::personal_calendar_id().to_vec(),
        since_modseq: None,
        after_event_id: None,
        limit: 0,
    };
    match caldav
        .query_events_decoded(query, &DavRecipientKeys::derive(DavKind::Calendar, msek))
        .await
    {
        Ok(DecodedEventsPage::Ok { events, .. }) => Ok(events),
        Ok(DecodedEventsPage::CalendarNotFound) => Ok(Vec::new()),
        Err(e) => Err(err(format!("read the Personal calendar: {e:?}"))),
    }
}

/// CAL-ADDRESS → principal over the addresses a test registered, all homed
/// on the one nest (the empty home the drain reports for a same-nest
/// channel). Stands in for the anon `by_handle` discovery, which needs DNS the
/// harness has none of — the stand-in the Rust pin uses too; every authority
/// decision on top of it is the shared apply's.
struct HarnessDirectory(Vec<(String, String)>);

impl fauna_client_caldav::PrincipalResolver for HarnessDirectory {
    async fn resolve_principal(
        &self,
        caladdr: &str,
    ) -> Option<fauna_client_caldav::SchedulingPrincipal> {
        let addr = caladdr.trim();
        let addr = addr
            .strip_prefix("mailto:")
            .or_else(|| addr.strip_prefix("MAILTO:"))
            .unwrap_or(addr);
        self.0
            .iter()
            .find(|(a, _)| a.eq_ignore_ascii_case(addr))
            .map(|(_, actor)| fauna_client_caldav::SchedulingPrincipal {
                actor_id: actor.clone(),
                home_nest_url: String::new(),
            })
    }
}

/// The drain's hand-off, kept: each iMIP with the origin the real nest
/// reported for it. The apply runs after the drain, so the outcome the
/// shared apply returns reaches the caller (the production
/// `NestSchedulingSink` applies inline and reports only a log line).
#[derive(Default)]
struct KeptImips(Mutex<Vec<(Vec<u8>, fauna_conversations::backend::SchedulingOrigin)>>);

#[async_trait::async_trait]
impl fauna_conversations::backend::SchedulingSink for KeptImips {
    async fn apply_scheduling_imip(
        &self,
        raw_rfc5322: Vec<u8>,
        origin: fauna_conversations::backend::SchedulingOrigin,
    ) -> Result<(), String> {
        self.0
            .lock()
            .expect("kept iMIPs poisoned")
            .push((raw_rfc5322, origin));
        Ok(())
    }
}

/// The recipient's inbox apply: a `scheduling` Welcome joins through the
/// shared [`ingest_scheduling_welcome`] — the calendar-apply drain, never a
/// chat thread — exactly as a client's receive session routes it. Every other
/// kind is left un-acked, never dropped.
///
/// [`ingest_scheduling_welcome`]: fauna_conversations::backends::fauna_mls::ingest_scheduling_welcome
struct SchedulingInboxApply<'a> {
    backend: &'a fauna_conversations::backends::fauna_mls::FaunaMlsBackend,
    joined: &'a Mutex<Vec<fauna_mls::types::ChannelId>>,
}

impl fauna_client_inbox::InboxApply for SchedulingInboxApply<'_> {
    type Error = String;

    async fn apply_welcome(
        &self,
        welcome: fauna_protocol::inbox::WelcomeInbox,
    ) -> Result<(), Self::Error> {
        if welcome.channel_type.as_deref() != Some("scheduling") {
            return Err("the scheduling recipient joins scheduling Welcomes only".into());
        }
        let channel_hex = welcome.channel_id.unwrap_or_default();
        let channel = fauna_conversations::backends::fauna_mls::ingest_scheduling_welcome(
            self.backend,
            &channel_hex,
            &welcome.welcome_bytes,
            welcome.nest_url.as_deref().unwrap_or_default(),
        )
        .await
        .map_err(|e| e.to_string())?;
        self.joined.lock().expect("joined poisoned").push(channel);
        Ok(())
    }

    async fn apply_contact_request(&self, _tuple_bytes: Vec<u8>) -> Result<(), Self::Error> {
        Err("the scheduling recipient joins scheduling Welcomes only".into())
    }

    async fn apply_security_notice(
        &self,
        _notice: fauna_protocol::inbox::SecurityNoticeInbox,
    ) -> Result<(), Self::Error> {
        Err("the scheduling recipient joins scheduling Welcomes only".into())
    }
}

/// Carry `imip` from `sender` to `recipient` over the **sealed scheduling
/// rail** and hand back what the recipient's drain received: the recipient
/// (an in-memory MLS engine for this call) uploads one key package, the
/// sender's shared [`deliver_scheduling_imip`] builds the one-off group and
/// posts the iMIP as its first application message, and the recipient drains
/// its inbox (the Welcome joins through [`SchedulingInboxApply`]) and walks
/// the joined channel with the shared [`poll_inbound_scheduling`] — until the
/// one iMIP and the author the nest attests for it are in hand.
///
/// [`deliver_scheduling_imip`]: fauna_conversations::backends::fauna_mls::FaunaMlsBackend::deliver_scheduling_imip
/// [`poll_inbound_scheduling`]: fauna_conversations::backends::fauna_mls::poll_inbound_scheduling
async fn deliver_over_the_sealed_rail(
    sender_nest: &Arc<NestClient>,
    sender: &ActorKeypair,
    recipient_nest: &Arc<NestClient>,
    recipient: &ActorKeypair,
    imip: Vec<u8>,
) -> Result<(Vec<u8>, fauna_client_caldav::InboundOrigin), FfiError> {
    use fauna_conversations::backends::fauna_mls::{FaunaMlsBackend, poll_inbound_scheduling};
    use fauna_mls::engine::MlsEngine;

    let engine_of = |who: &ActorKeypair, what: &str| {
        MlsEngine::new_in_memory(who.clone_keypair())
            .map(Arc::new)
            .map_err(|e| err(format!("the {what}'s MLS engine: {e}")))
    };
    let backend_of = |nest: &Arc<NestClient>, engine: Arc<MlsEngine>, who: &ActorKeypair| {
        let rpc = Arc::new(fauna_client_conversations::NestConversationsRpc::new(
            Arc::clone(nest),
        ));
        (
            FaunaMlsBackend::new(engine, rpc.clone(), "e2e-scheduling", who.actor_id()),
            rpc,
        )
    };

    let recipient_engine = engine_of(recipient, "recipient")?;
    let (recipient_backend, recipient_rpc) =
        backend_of(recipient_nest, Arc::clone(&recipient_engine), recipient);
    // Exactly one package: the delivery below consumes it, so no package of
    // an engine that dies with this call outlives it to be picked next time.
    let package = recipient_engine
        .generate_key_packages_bytes(1)
        .map_err(|e| err(format!("mint the recipient's key package: {e}")))?;
    recipient_rpc
        .keypackage_upload(package, false)
        .await
        .map_err(|e| err(format!("upload the recipient's key package: {e}")))?;

    let (sender_backend, _) = backend_of(sender_nest, engine_of(sender, "sender")?, sender);
    sender_backend
        .deliver_scheduling_imip(recipient.actor_id(), None, imip)
        .await
        .map_err(|e| err(format!("deliver over the scheduling rail: {e}")))?;

    let inbox = fauna_client_inbox::InboxClient::new(Arc::clone(recipient_nest));
    let joined = Mutex::new(Vec::new());
    let apply = SchedulingInboxApply {
        backend: &recipient_backend,
        joined: &joined,
    };
    let kept = KeptImips::default();
    let mut cursors: HashMap<String, i64> = HashMap::new();
    let deadline = tokio::time::Instant::now() + SCHEDULING_RECEIVE_BUDGET;
    loop {
        fauna_client_inbox::drain(&inbox, &apply, 0)
            .await
            .map_err(|e| err(format!("the recipient's inbox drain: {e}")))?;
        let channels = joined.lock().expect("joined poisoned").clone();
        for channel in &channels {
            let after = cursors.entry(channel.to_string()).or_insert(0);
            poll_inbound_scheduling(&recipient_backend, &kept, channel, after, 0)
                .await
                .map_err(|e| err(format!("walk scheduling channel {channel}: {e}")))?;
        }
        let got = std::mem::take(&mut *kept.0.lock().expect("kept iMIPs poisoned"));
        if got.len() > 1 {
            return Err(err(format!(
                "one delivery drained {} iMIPs, expected exactly one",
                got.len()
            )));
        }
        if let Some((raw, origin)) = got.into_iter().next() {
            return Ok((
                raw,
                fauna_client_caldav::InboundOrigin {
                    author: origin.author,
                    home_nest_url: origin.home_nest_url,
                },
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(err(format!(
                "the delivered iMIP did not reach the recipient within {}s \
                 (scheduling channels joined: {})",
                SCHEDULING_RECEIVE_BUDGET.as_secs(),
                channels.len()
            )));
        }
        tokio::time::sleep(SCHEDULING_POLL_PACE).await;
    }
}

/// What the recipient's apply did, as the dag-cbor array the Python wrapper
/// reads: `[method, outcome, reason, author]` — `method` `REQUEST` / `CANCEL`
/// / `REPLY` (or `null` when the message was not scheduling), `outcome` the
/// apply's terminal in snake case (`created`, `updated`, `cancelled`,
/// `applied`, `refused`, `no_matching_event`, `no_matching_attendee`,
/// `no_uid`, `not_scheduling`), `reason` the refusal's wire reason or `null`,
/// `author` the record author the nest attested (sealed rail) or `null`.
fn scheduling_outcome_cbor(
    method: Option<&str>,
    outcome: fauna_client_caldav::SchedulingApplyOutcome,
    author: Option<String>,
) -> Result<Outcome, FfiError> {
    use fauna_client_caldav::SchedulingApplyOutcome as S;
    use fauna_client_caldav::{InboundReplyOutcome as Rp, InboundRequestOutcome as Rq};
    let (terminal, reason) = match &outcome {
        S::Request(Rq::Created { .. }) => ("created", None),
        S::Request(Rq::Updated { .. }) => ("updated", None),
        S::Request(Rq::Cancelled { .. }) => ("cancelled", None),
        S::Request(Rq::Refused { reason, .. }) | S::Reply(Rp::Refused { reason, .. }) => {
            ("refused", Some(reason.as_wire().to_string()))
        }
        S::Request(Rq::NoMatchingEvent) | S::Reply(Rp::NoMatchingEvent) => {
            ("no_matching_event", None)
        }
        S::Request(Rq::NoUid) | S::Reply(Rp::NoUid) => ("no_uid", None),
        S::Reply(Rp::Applied { .. }) => ("applied", None),
        S::Reply(Rp::NoMatchingAttendee) => ("no_matching_attendee", None),
        S::Reply(Rp::NotCalendarReply) | S::NotScheduling => ("not_scheduling", None),
    };
    fauna_core::encoding::canonical_encode(&(
        method.map(str::to_string),
        terminal.to_string(),
        reason,
        author,
    ))
    .map(Outcome::Ok)
    .map_err(encode_err)
}

/// Store an event the way the **organizer's** client does: its `Personal`
/// calendar provisioned (idempotently), then the event sealed and put through
/// the shared [`CalDavClient::seal_and_put_event`] with every named attendee
/// `NEEDS-ACTION` — the organizer's copy an inbound `REPLY` later merges
/// into.
///
/// `request` is the dag-cbor array `[msek, uid, summary, dtstart, dtend,
/// organizer, attendees, timestamp]`: `msek` the actor's 32-byte calendar
/// key (client-held, any 32 bytes), `dtstart` / `dtend` iCalendar UTC times
/// (`20361102T100000Z`), `attendees` bare addresses, `timestamp` epoch
/// seconds. `out` receives `{"ok": PutEventCiphertextReply}` or `{"err":
/// RpcError}`.
///
/// [`CalDavClient::seal_and_put_event`]: fauna_client_caldav::CalDavClient::seal_and_put_event
///
/// # Safety
///
/// As `fauna_harness_create_set`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_caldav_put_event(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_caldav::{CalDavClient, PutEventError, personal_calendar_id, uid_hash};
    use fauna_protocol::bridge_routing::{ProvisionCalendarReply, ProvisionCalendarRequest};

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        #[allow(clippy::type_complexity)]
        let (msek, uid, summary, dtstart, dtend, organizer, attendees, timestamp): (
            fauna_protocol::ByteBuf,
            String,
            String,
            String,
            String,
            String,
            Vec<String>,
            i64,
        ) = decode_request!(call.req)?;
        let msek = msek_from(&msek, "msek")?;
        let actor = call.identity.actor_id().0;
        with_client(call.nest_url, call.identity, |nest| async move {
            let caldav = CalDavClient::new(Arc::clone(&nest));
            let personal = personal_calendar_id();
            let metadata = fauna_client_caldav::seal_event_body(b"{\"name\":\"Personal\"}", &msek)
                .map_err(|e| err(format!("seal the calendar's metadata: {e:?}")))?;
            match caldav
                .provision_calendar(ProvisionCalendarRequest {
                    actor_id: actor.to_vec(),
                    calendar_id: personal.to_vec(),
                    encrypted_metadata: metadata,
                    ..Default::default()
                })
                .await
            {
                Ok(ProvisionCalendarReply::Created | ProvisionCalendarReply::AlreadyExists) => {}
                Ok(other) => return Err(err(format!("provision Personal: {other:?}"))),
                Err(e) => return outcome!(Err::<(), _>(e)),
            }
            let event = event_fields(uid.clone(), summary, dtstart, dtend);
            let reply = caldav
                .seal_and_put_event(
                    &actor,
                    &personal,
                    &uid_hash(&uid),
                    &msek,
                    &event,
                    &invited_roster(&attendees),
                    &organizer,
                    None,
                    timestamp,
                    None,
                )
                .await;
            match reply {
                Ok(reply) => outcome!(Ok::<_, NestClientError>(reply)),
                Err(PutEventError::Transport(e)) => outcome!(Err::<(), _>(e)),
                Err(e) => Err(err(format!("seal and put the event: {e:?}"))),
            }
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Read the actor's `Personal` calendar back the way its client does —
/// through the shared [`CalDavClient::query_events_decoded`] under its key.
///
/// `request` is the dag-cbor array `[msek]`. `out` receives `{"ok": [[uid,
/// etag, summary, [[email, partstat], …], ics], …]}` — one entry per stored
/// event, `uid` the event's iCalendar UID, `ics` the whole decoded body, so
/// a test can tell a byte-for-byte untouched event from a rewritten one; an
/// empty list when the calendar does not exist yet.
///
/// [`CalDavClient::query_events_decoded`]: fauna_client_caldav::CalDavClient::query_events_decoded
///
/// # Safety
///
/// As `fauna_harness_create_set`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_caldav_events(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        let (msek,): (fauna_protocol::ByteBuf,) = decode_request!(call.req)?;
        let msek = msek_from(&msek, "msek")?;
        let actor = call.identity.actor_id().0;
        with_client(call.nest_url, call.identity, |nest| async move {
            let caldav = fauna_client_caldav::CalDavClient::new(Arc::clone(&nest));
            let events = personal_events(&caldav, &actor, &msek).await?;
            let rows: Vec<EventRow> = events
                .into_iter()
                .map(|e| {
                    let fields = fauna_core::ical::parse_ical(&e.ics).unwrap_or_default();
                    let roster = fauna_client_caldav::parse_ical_attendees(&e.ics)
                        .into_iter()
                        .map(|a| (a.email, a.partstat))
                        .collect();
                    (fields.uid, e.etag, fields.summary, roster, e.ics)
                })
                .collect();
            fauna_core::encoding::canonical_encode(&rows)
                .map(Outcome::Ok)
                .map_err(encode_err)
        })
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// Send one **organizer-shaped iMIP** (`REQUEST` or `CANCEL`) from the actor
/// `secret` names to a recipient over the sealed scheduling rail, and apply
/// it on the **recipient's** client: the message built by the shared
/// [`build_event_imip`] (so whoever sends it, it is byte-for-byte what the
/// named organizer's client would send), carried by
/// [`deliver_over_the_sealed_rail`], then applied through the shared
/// [`CalDavClient::apply_inbound_scheduling_from_message`] under the origin
/// the nest attested — the apply that creates, updates, cancels or refuses.
///
/// `request` is the dag-cbor array `[recipient_secret, recipient_msek,
/// method, uid, summary, dtstart, dtend, organizer, attendees, directory,
/// timestamp]`: `method` `REQUEST` or `CANCEL`, `organizer` the address the
/// message's `ORGANIZER` names (the sender's own, or — for a forgery —
/// someone else's), `directory` `[[address, actor_hex], …]` (the
/// [`HarnessDirectory`]). `out` receives `{"ok": [method, outcome, reason,
/// author]}` ([`scheduling_outcome_cbor`]).
///
/// [`build_event_imip`]: fauna_client_caldav::build_event_imip
/// [`CalDavClient::apply_inbound_scheduling_from_message`]: fauna_client_caldav::CalDavClient::apply_inbound_scheduling_from_message
///
/// # Safety
///
/// As `fauna_harness_create_set`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_scheduling_invite(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_caldav::{CalDavClient, ITipMethod, build_event_imip, epoch_secs_to_ical_utc};

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        #[allow(clippy::type_complexity)]
        let (
            recipient_secret,
            recipient_msek,
            method,
            uid,
            summary,
            dtstart,
            dtend,
            organizer,
            attendees,
            directory,
            timestamp,
        ): (
            fauna_protocol::ByteBuf,
            fauna_protocol::ByteBuf,
            String,
            String,
            String,
            String,
            String,
            String,
            Vec<String>,
            Vec<(String, String)>,
            i64,
        ) = decode_request!(call.req)?;
        let recipient =
            ActorKeypair::from_secret(msek_from(&recipient_secret, "recipient_secret")?);
        let recipient_msek = msek_from(&recipient_msek, "recipient_msek")?;
        let (method, method_name) = match method.to_ascii_uppercase().as_str() {
            "REQUEST" => (ITipMethod::Request, "REQUEST"),
            "CANCEL" => (ITipMethod::Cancel, "CANCEL"),
            other => {
                return Err(err(format!(
                    "method must be REQUEST or CANCEL, got {other:?}"
                )));
            }
        };
        let imip = build_event_imip(
            method,
            &event_fields(uid, summary, dtstart, dtend),
            &invited_roster(&attendees),
            &organizer,
            &epoch_secs_to_ical_utc(timestamp),
        )
        .ok_or_else(|| err("the roster names no one to send the iMIP to"))?
        .raw_rfc5322;
        let sender = call.identity;
        let nest_url = call.nest_url;
        with_client(
            nest_url.clone(),
            recipient.clone_keypair(),
            |recipient_nest| async move {
                let (raw, origin) =
                    with_sender(&nest_url, &sender, &recipient_nest, &recipient, imip).await?;
                let caldav = CalDavClient::new(Arc::clone(&recipient_nest));
                let applied = caldav
                    .apply_inbound_scheduling_from_message(
                        &recipient.actor_id().0,
                        &recipient_msek,
                        &[],
                        &raw,
                        timestamp,
                        &origin,
                        &HarnessDirectory(directory),
                    )
                    .await
                    .map_err(|e| err(format!("the recipient's apply: {e:?}")))?;
                scheduling_outcome_cbor(Some(method_name), applied, origin.author)
            },
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}

/// [`deliver_over_the_sealed_rail`] with the sender's own connection, opened
/// beside the recipient's (both nest calls run on the one runtime
/// [`with_client`] blocks on, so the sender's dial is made here rather than
/// through a second `block_on`).
async fn with_sender(
    nest_url: &str,
    sender: &ActorKeypair,
    recipient_nest: &Arc<NestClient>,
    recipient: &ActorKeypair,
    imip: Vec<u8>,
) -> Result<(Vec<u8>, fauna_client_caldav::InboundOrigin), FfiError> {
    let auth = Arc::new(AuthClient::new(
        nest_url.to_string(),
        sender.clone_keypair(),
    ));
    let sender_nest = Arc::new(NestClient::with_auth(auth));
    sender_nest
        .connect_and_wait(crate::nest_client::CONNECT_TIMEOUT)
        .await
        .map_err(|e| err(format!("connect the sender: {e}")))?;
    let result =
        deliver_over_the_sealed_rail(&sender_nest, sender, recipient_nest, recipient, imip).await;
    sender_nest.disconnect().await;
    result
}

/// An attendee's **RSVP** as its client sends it, and the **organizer's**
/// client applying it: the answer folded into the event the sender holds
/// (the shared [`apply_rsvp`] over the copy read back from the sender's own
/// `Personal`) and turned into the `REPLY` by the shared
/// [`imip_reply_for_rsvp`] — on behalf of `as_address`, which is the
/// sender's own rostered address for a genuine reply, or another attendee's
/// for a forgery — then carried to the organizer and applied by the shared
/// apply on the organizer's side.
///
/// `rail` picks the carrier: `"sealed"` — [`deliver_over_the_sealed_rail`]
/// and [`CalDavClient::apply_inbound_scheduling_from_message`] under the
/// author the nest attested; `"mail"` — the copy the organizer's mail door
/// files, stamped `X-Fauna-Authenticated-Sender: <door_sender>` by the
/// door's own [`build_authenticated_sender_stamp`] (`door_sender` the
/// address the door authenticated; `null` files it unstamped), applied
/// through [`CalDavClient::apply_inbound_reply_from_mail`].
///
/// `request` is the dag-cbor array `[sender_msek, recipient_secret,
/// recipient_msek, uid, as_address, response, rail, door_sender, directory,
/// timestamp]`: `response` `going` / `interested` / `declined`. `out`
/// receives `{"ok": [method, outcome, reason, author]}`
/// ([`scheduling_outcome_cbor`]; `author` is `null` on the mail rail).
///
/// [`apply_rsvp`]: fauna_client_caldav::apply_rsvp
/// [`imip_reply_for_rsvp`]: fauna_client_caldav::imip_reply_for_rsvp
/// [`build_authenticated_sender_stamp`]: fauna_mail::sender_auth::build_authenticated_sender_stamp
/// [`CalDavClient::apply_inbound_scheduling_from_message`]: fauna_client_caldav::CalDavClient::apply_inbound_scheduling_from_message
/// [`CalDavClient::apply_inbound_reply_from_mail`]: fauna_client_caldav::CalDavClient::apply_inbound_reply_from_mail
///
/// # Safety
///
/// As `fauna_harness_create_set`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fauna_harness_scheduling_rsvp(
    nest_url: *const c_char,
    secret: *const u8,
    secret_len: u32,
    request: *const u8,
    request_len: u32,
    out: *mut FfiBuffer,
) -> i32 {
    use fauna_client_caldav::{
        CalDavClient, SchedulingApplyOutcome, apply_rsvp, imip_reply_for_rsvp,
    };

    let body = || {
        // SAFETY: per the `# Safety` section above.
        let call = unsafe { decode_call(nest_url, secret, secret_len, request, request_len)? };
        #[allow(clippy::type_complexity)]
        let (
            sender_msek,
            recipient_secret,
            recipient_msek,
            uid,
            as_address,
            response,
            rail,
            door_sender,
            directory,
            timestamp,
        ): (
            fauna_protocol::ByteBuf,
            fauna_protocol::ByteBuf,
            fauna_protocol::ByteBuf,
            String,
            String,
            String,
            String,
            Option<String>,
            Vec<(String, String)>,
            i64,
        ) = decode_request!(call.req)?;
        let sender_msek = msek_from(&sender_msek, "sender_msek")?;
        let recipient =
            ActorKeypair::from_secret(msek_from(&recipient_secret, "recipient_secret")?);
        let recipient_msek = msek_from(&recipient_msek, "recipient_msek")?;
        if rail != "sealed" && rail != "mail" {
            return Err(err(format!(
                "rail must be \"sealed\" or \"mail\", got {rail:?}"
            )));
        }
        let sender = call.identity;
        let sender_actor = sender.actor_id().0;
        let nest_url = call.nest_url;

        // The sender's client: its held copy, the answer, the REPLY.
        let reply = with_client(
            nest_url.clone(),
            sender.clone_keypair(),
            |nest| async move {
                let caldav = CalDavClient::new(Arc::clone(&nest));
                let held = personal_events(&caldav, &sender_actor, &sender_msek)
                    .await?
                    .into_iter()
                    .find(|e| fauna_core::ical::parse_ical(&e.ics).is_ok_and(|f| f.uid == uid))
                    .ok_or_else(|| err(format!("the sender holds no event {uid:?} to answer")))?;
                let rewrite =
                    apply_rsvp(&held.ics, held.fauna_ext.as_ref(), &as_address, &response)
                        .map_err(|e| err(format!("apply the RSVP: {e}")))?;
                imip_reply_for_rsvp(&rewrite, &as_address, timestamp)
                    .map(|m| m.raw_rfc5322)
                    .ok_or_else(|| {
                        err(format!(
                            "{as_address} answering {uid:?} has no organizer to reply to"
                        ))
                    })
            },
        )?;

        with_client(
            nest_url.clone(),
            recipient.clone_keypair(),
            |recipient_nest| async move {
                let caldav = CalDavClient::new(Arc::clone(&recipient_nest));
                let actor = recipient.actor_id().0;
                if rail == "mail" {
                    let mut filed = Vec::new();
                    if let Some(addr) = door_sender {
                        let stamp = fauna_mail::sender_auth::build_authenticated_sender_stamp(addr);
                        if stamp.is_empty() {
                            return Err(err("the door cannot stamp that sender address"));
                        }
                        filed.extend_from_slice(stamp.as_bytes());
                        filed.extend_from_slice(b"\r\n");
                    }
                    filed.extend_from_slice(&reply);
                    let applied = caldav
                        .apply_inbound_reply_from_mail(
                            &actor,
                            &recipient_msek,
                            &[],
                            &filed,
                            timestamp,
                        )
                        .await
                        .map_err(|e| err(format!("the organizer's mail apply: {e:?}")))?;
                    return scheduling_outcome_cbor(
                        Some("REPLY"),
                        SchedulingApplyOutcome::Reply(applied),
                        None,
                    );
                }
                let (raw, origin) =
                    with_sender(&nest_url, &sender, &recipient_nest, &recipient, reply).await?;
                let applied = caldav
                    .apply_inbound_scheduling_from_message(
                        &actor,
                        &recipient_msek,
                        &[],
                        &raw,
                        timestamp,
                        &origin,
                        &HarnessDirectory(directory),
                    )
                    .await
                    .map_err(|e| err(format!("the organizer's apply: {e:?}")))?;
                scheduling_outcome_cbor(Some("REPLY"), applied, origin.author)
            },
        )
    };
    // SAFETY: per the `# Safety` section above.
    unsafe { run(body, out) }
}
